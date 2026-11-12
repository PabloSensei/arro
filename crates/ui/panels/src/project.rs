//! The project tree tool window on a virtual list.

use std::ops::Range;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use editor_view::{EditorEvent, EditorView, FolderMove, Semantic};
use futures_util::StreamExt;
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use workspace::{DockSide, FileIcons, PROJECT_TOOL, Workspace};

use crate::file_tree::{Child, FileTree, read_folder};
use crate::prompt::Prompt;
use crate::tree_ops::{
    Creation, FileTemplate, create_on_disk, delete_on_disk, plan_creation_with_template, plan_move,
    plan_paste, rename_on_disk, renamed, target_folder,
};

actions!(
    project_panel,
    [
        SelectPrevious,
        SelectNext,
        ExpandRow,
        CollapseRow,
        OpenRow,
        RenameRow,
        DeleteRow,
        SafeDeleteRow,
        NewItem,
        CutRows,
        CopyRows,
        PasteRows,
        CopyPaths,
        ExpandAllRows,
        CollapseAllRows,
        ExpandNode,
        CollapseNode,
        OpenInRightSplitRow,
        CompareTwoFiles
    ]
);

const CONTEXT: &str = "ProjectPanel";
const FOLDER_ICON: &str = "nodes/folder";
const ROOT_ICON: &str = "nodes/module";
const ROW_MARGIN: f32 = SPACE_L;
/// A pause this long starts a new speed search rather than adding to the old one.
const SEARCH_TIMEOUT: Duration = Duration::from_millis(1000);

/// A row on its way to another folder by the pointer.
#[derive(Clone)]
struct DraggedTree {
    path: PathBuf,
    folder: bool,
    name: SharedString,
}

struct TreeDragPreview(SharedString);

impl Render for TreeDragPreview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        div()
            .flex()
            .items_center()
            .h(px(24.))
            .px(px(8.))
            .rounded(px(4.))
            .border_1()
            .border_color(theme.tab_selected_border)
            .bg(theme.tab_selected)
            .text_color(theme.text)
            .child(self.0.clone())
    }
}

/// One key of the tree and the IDEA action it implements, if any.
#[cfg(test)]
pub(crate) struct PanelKey {
    pub(crate) keys: &'static str,
    pub(crate) action: &'static str,
    pub(crate) idea: Option<&'static str>,
}

/// The tree keys and, from the same rows, the table the keymap check reads.
macro_rules! panel_keys {
    ($(($keys:literal, $action:ident, $idea:expr)),* $(,)?) => {
        pub fn bind_keys(cx: &mut App) {
            cx.bind_keys([$(KeyBinding::new($keys, $action, Some(CONTEXT))),*]);
        }

        #[cfg(test)]
        pub(crate) const PANEL_KEYS: &[PanelKey] = &[
            $(PanelKey {
                keys: $keys,
                action: concat!("project_panel::", stringify!($action)),
                idea: $idea,
            }),*
        ];
    };
}

panel_keys![
    ("up", SelectPrevious, None),
    ("down", SelectNext, None),
    ("right", ExpandRow, None),
    ("left", CollapseRow, None),
    ("enter", OpenRow, None),
    ("f4", OpenRow, Some("EditSource")),
    ("shift-f6", RenameRow, Some("RenameElement")),
    ("delete", DeleteRow, Some("$Delete")),
    ("alt-delete", SafeDeleteRow, Some("SafeDelete")),
    ("alt-insert", NewItem, Some("NewElement")),
    ("ctrl-x", CutRows, Some("$Cut")),
    ("ctrl-c", CopyRows, Some("$Copy")),
    ("ctrl-v", PasteRows, Some("$Paste")),
    ("ctrl-shift-c", CopyPaths, Some("CopyPaths")),
    ("ctrl-=", ExpandAllRows, Some("ExpandAll")),
    ("ctrl--", CollapseAllRows, Some("CollapseAll")),
    ("numpad_add", ExpandNode, Some("ExpandTreeNode")),
    ("numpad_subtract", CollapseNode, Some("CollapseTreeNode")),
    ("shift-enter", OpenInRightSplitRow, Some("OpenInRightSplit")),
    ("ctrl-d", CompareTwoFiles, Some("CompareTwoFiles")),
];

/// What the tree can create, from the application and the basics.
pub struct FileTemplates(pub Vec<FileTemplate>);

impl Global for FileTemplates {}

/// Files of the project tree that Ctrl+C or Ctrl+X took, and whether they are to move.
#[derive(Default)]
struct TreeClipboard {
    paths: Vec<PathBuf>,
    cut: bool,
}

impl Global for TreeClipboard {}

/// Copies a file or a folder with everything in it. Blocks.
fn copy_path(from: &Path, to: &Path) -> std::io::Result<()> {
    if from.is_dir() {
        std::fs::create_dir_all(to)?;
        for entry in std::fs::read_dir(from)? {
            let entry = entry?;
            copy_path(&entry.path(), &to.join(entry.file_name()))?;
        }
        Ok(())
    } else {
        std::fs::copy(from, to).map(|_| ())
    }
}

/// Alt+F1 in the editor: shows the file of the tab in the project tree.
pub fn bind_actions(
    panel: Entity<ProjectPanel>,
    workspace: WeakEntity<Workspace>,
    window: AnyWindowHandle,
    cx: &mut App,
) {
    cx.on_action(move |_: &workspace::SelectInProject, cx| {
        let (panel, workspace) = (panel.clone(), workspace.clone());
        // The window is being updated while its action runs, so the rest waits.
        cx.defer(move |cx| {
            window
                .update(cx, |_, window, cx| {
                    let open = workspace.update(cx, |this, _| this.active_editor());
                    let Some((path, _)) = open.ok().flatten() else {
                        return;
                    };
                    let show = |this: &mut Workspace, cx: &mut Context<Workspace>| {
                        this.show_tool(PROJECT_TOOL, window, cx)
                    };
                    workspace.update(cx, show).ok();
                    panel.update(cx, |this, cx| this.select_path(&path, cx));
                })
                .ok();
        });
    });
}

/// The panel of the window, for scripts.
pub struct ActiveProjectPanel(pub Entity<ProjectPanel>);

impl Global for ActiveProjectPanel {}

pub struct ProjectPanel {
    tree: FileTree,
    /// The rows selected with Ctrl and Shift, the row the menu acts on last.
    selection: Vec<PathBuf>,
    /// The row a Shift selection grows from.
    anchor: Option<usize>,
    scroll: UniformListScrollHandle,
    focus: FocusHandle,
    workspace: WeakEntity<Workspace>,
    icons: FileIcons,
    templates: Vec<FileTemplate>,
    /// A path to select once its folder has been read.
    pending_select: Option<PathBuf>,
    /// The letters typed for the speed search.
    search: String,
    search_at: Option<Instant>,
    /// Bumped after a folder listing reaches the tree, so a test can wait for the walk
    /// of the project by its signal rather than by a clock.
    listings: u64,
}

impl ProjectPanel {
    /// `workspace` is still being built, so everything comes from the arguments.
    pub fn create(
        workspace: &Workspace,
        weak: WeakEntity<Workspace>,
        _: &mut Window,
        cx: &mut App,
    ) -> Entity<Self> {
        let layout = workspace.layout();
        let mut tree = FileTree::new(
            workspace.root().to_path_buf(),
            layout.tree_expanded.iter().cloned(),
        );
        // Scratches live outside the project but show up in its tree.
        let scratches = workspace.state_dir().join("scratches");
        tree.add_virtual_child(
            workspace.root(),
            Child {
                path: scratches,
                name: "Scratches".to_owned(),
                folder: true,
            },
        );
        let selected = layout.tree_selected.clone();
        let selection: Vec<PathBuf> = selected.iter().cloned().collect();
        let icons = workspace.icons().clone();
        let templates = cx
            .try_global::<FileTemplates>()
            .map_or_else(Vec::new, |known| known.0.clone());
        cx.new(|cx| {
            if let Some(git) = cx
                .try_global::<crate::git::ActiveGit>()
                .map(|g| g.0.clone())
            {
                // Colours follow the status of the repository.
                cx.observe(&git, |_, _, cx| cx.notify()).detach();
            }
            let mut panel = Self {
                tree,
                selection,
                anchor: None,
                scroll: UniformListScrollHandle::new(),
                focus: cx.focus_handle(),
                workspace: weak,
                icons,
                templates,
                pending_select: None,
                search: String::new(),
                search_at: None,
                listings: 0,
            };
            panel.read_pending(cx);
            panel
        })
    }

    pub fn focus(&self) -> FocusHandle {
        self.focus.clone()
    }

    /// The path the tree acts on, if any.
    pub fn selected_path(&self) -> Option<&Path> {
        self.selection.last().map(PathBuf::as_path)
    }

    /// Every selected path, in the order the tree shows them. A row whose folder is
    /// still being read is kept in the order it was selected.
    pub fn selected_paths(&self) -> Vec<PathBuf> {
        let selection = &self.selection;
        let mut paths: Vec<PathBuf> = self
            .tree
            .rows()
            .iter()
            .filter(|row| selection.contains(&row.path))
            .map(|row| row.path.clone())
            .collect();
        if paths.is_empty() {
            paths.clone_from(selection);
        }
        paths
    }

    /// How far the tree is scrolled down, for the tests of the bars.
    #[cfg(test)]
    pub(crate) fn scroll_offset(&self) -> f32 {
        f32::from(-self.scroll.0.borrow().base_handle.offset().y)
    }

    /// Where a popup of the selected row goes: the bottom left corner of the row, in
    /// window coordinates. Before the first frame the panel itself is the fallback.
    pub fn selected_row_position(&self) -> Point<Pixels> {
        let index = self.selected_index().unwrap_or(0);
        let handle = self.scroll.0.borrow();
        if let Some(bounds) = handle.base_handle.bounds_for_item(index) {
            return point(bounds.left() + px(8.), bounds.bottom() - px(4.));
        }
        let viewport = handle.base_handle.bounds();
        let item = px(ROW_HEIGHT);
        let offset = handle.base_handle.offset();
        let top = viewport.top() + offset.y + item * index as f32;
        point(viewport.left() + px(8.), top + item - px(4.))
    }

    /// Ctrl+D: compares the selected file with the clipboard.
    /// Ctrl+D: two selected files against each other, one against the clipboard.
    fn compare_with_clipboard(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let selected = self.selected_paths();
        let Some(git) = cx
            .try_global::<crate::git::ActiveGit>()
            .map(|git| git.0.clone())
        else {
            return;
        };
        if let [left, right] = selected.as_slice() {
            crate::diff_view::DiffView::compare_files(
                git,
                self.workspace.clone(),
                left.clone(),
                right.clone(),
                window,
                cx,
            );
            return;
        }
        let [path] = selected.as_slice() else {
            return;
        };
        let path = path.clone();
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
            let workspace = self.workspace.clone();
            workspace
                .update(cx, |workspace, cx| {
                    workspace.notify_user("Compare", "The clipboard is empty", cx)
                })
                .ok();
            return;
        };
        crate::diff_view::DiffView::compare_clipboard(
            git,
            self.workspace.clone(),
            path,
            text,
            window,
            cx,
        );
    }

    pub fn tree(&self) -> &FileTree {
        &self.tree
    }

    /// How many folder listings the background reader has handed to the tree. A test
    /// waits for this to grow instead of waiting for a clock.
    #[allow(dead_code)]
    pub fn listing_generation(&self) -> u64 {
        self.listings
    }

    fn read_pending(&mut self, cx: &mut Context<Self>) {
        for folder in self.tree.pending() {
            // A row stays empty until its listing arrives; a failure leaves it empty.
            self.tree.set_listing(folder.clone(), Vec::new());
            let path = folder.clone();
            let read = cx
                .background_executor()
                .spawn(async move { read_folder(&path) });
            cx.spawn(async move |this, cx| {
                let result = read.await;
                this.update(cx, |this, cx| this.apply_listing(folder, result, cx))
                    .ok();
            })
            .detach();
        }
    }

    /// The folder changed on disk: reads it again if the tree shows it.
    pub fn refresh_folder(&mut self, folder: &Path, cx: &mut Context<Self>) {
        if !self.tree.has_listing(folder) {
            return;
        }
        let folder = folder.to_path_buf();
        let path = folder.clone();
        let read = cx
            .background_executor()
            .spawn(async move { read_folder(&path) });
        cx.spawn(async move |this, cx| {
            let result = read.await;
            this.update(cx, |this, cx| this.apply_listing(folder, result, cx))
                .ok();
        })
        .detach();
    }

    /// Reads the open folders again: files came and went behind the back of the tree.
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        let rows = self.tree.rows().iter().filter(|row| row.open);
        let open: Vec<PathBuf> = rows.map(|row| row.path.clone()).collect();
        for folder in open {
            let path = folder.clone();
            let read = cx
                .background_executor()
                .spawn(async move { read_folder(&path) });
            cx.spawn(async move |this, cx| {
                let result = read.await;
                this.update(cx, |this, cx| this.apply_listing(folder, result, cx))
                    .ok();
            })
            .detach();
        }
    }

    fn apply_listing(
        &mut self,
        folder: PathBuf,
        result: std::io::Result<Vec<crate::file_tree::Child>>,
        cx: &mut Context<Self>,
    ) {
        match result {
            Ok(children) => {
                self.tree.set_listing(folder, children);
                self.listings += 1;
                self.read_pending(cx);
                if let Some(path) = self.pending_select.clone()
                    && let Some(index) = self.tree.index_of(&path)
                {
                    self.pending_select = None;
                    self.select(index, cx);
                }
            }
            Err(error) => {
                let text = format!("{}: {error}", folder.display());
                self.workspace
                    .update(cx, |workspace, cx| {
                        workspace.notify_user("Cannot read folder", text, cx)
                    })
                    .ok();
            }
        }
        cx.notify();
    }

    fn state_changed(&mut self, cx: &mut Context<Self>) {
        let expanded = self.tree.expanded();
        let selected = self.selected_path().map(Path::to_path_buf);
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.set_tree_state(expanded, selected, cx)
            })
            .ok();
        self.read_pending(cx);
        cx.notify();
    }

    pub fn toggle(&mut self, folder: &Path, cx: &mut Context<Self>) {
        self.tree.toggle(folder);
        self.state_changed(cx);
    }

    /// Expands every folder from the root down to `path`.
    pub fn reveal(&mut self, path: &Path, cx: &mut Context<Self>) {
        let root = self.tree.rows()[0].path.clone();
        for folder in path.ancestors().filter(|folder| folder.starts_with(&root)) {
            self.tree.set_expanded(folder, true);
        }
        self.state_changed(cx);
    }

    fn select(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(row) = self.tree.rows().get(index) else {
            return;
        };
        self.selection = vec![row.path.clone()];
        self.anchor = Some(index);
        self.scroll.scroll_to_item(index, ScrollStrategy::Nearest);
        self.state_changed(cx);
    }

    /// Ctrl+click: adds the row to the selection, or takes it out.
    pub(crate) fn toggle_selection(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(row) = self.tree.rows().get(index) else {
            return;
        };
        let path = row.path.clone();
        if let Some(at) = self.selection.iter().position(|known| *known == path) {
            if self.selection.len() > 1 {
                self.selection.remove(at);
            }
        } else {
            self.selection.push(path);
        }
        self.anchor = Some(index);
        self.scroll.scroll_to_item(index, ScrollStrategy::Nearest);
        self.state_changed(cx);
    }

    /// Shift+click and Shift+arrows: every row from the anchor to `index`.
    fn extend_selection(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(row) = self.tree.rows().get(index) else {
            return;
        };
        let path = row.path.clone();
        let anchor = self.anchor.unwrap_or(index).min(self.tree.rows().len() - 1);
        let (from, to) = if anchor <= index {
            (anchor, index)
        } else {
            (index, anchor)
        };
        self.selection = self.tree.rows()[from..=to]
            .iter()
            .map(|row| row.path.clone())
            .collect();
        // The row the caret is on stays the one the menu acts on.
        self.selection.retain(|known| *known != path);
        self.selection.push(path);
        self.scroll.scroll_to_item(index, ScrollStrategy::Nearest);
        self.state_changed(cx);
    }

    fn selected_index(&self) -> Option<usize> {
        self.tree.index_of(self.selected_path()?)
    }

    fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        let last = self.tree.rows().len() as isize - 1;
        let next = match self.selected_index() {
            Some(index) => (index as isize + delta).clamp(0, last),
            None => 0,
        };
        self.select(next as usize, cx);
    }

    /// Shift+Up and Shift+Down grow the selection from the row it started at.
    fn extend_selection_by(&mut self, delta: isize, cx: &mut Context<Self>) {
        let last = self.tree.rows().len() as isize - 1;
        let next = match self.selected_index() {
            Some(index) => (index as isize + delta).clamp(0, last),
            None => 0,
        };
        self.extend_selection(next as usize, cx);
    }

    /// Ctrl+Equal: every folder opens, the files keep their places.
    pub fn expand_all(&mut self, cx: &mut Context<Self>) {
        let folders: Vec<PathBuf> = self
            .tree
            .rows()
            .iter()
            .filter(|row| row.folder)
            .map(|row| row.path.clone())
            .collect();
        for folder in folders {
            self.tree.set_expanded(&folder, true);
        }
        self.state_changed(cx);
    }

    /// The same commands from the window, whose own update is already running: they
    /// change the tree and leave saving the layout to the caller.
    pub fn expand_all_quiet(&mut self, cx: &mut Context<Self>) {
        let folders: Vec<PathBuf> = self
            .tree
            .rows()
            .iter()
            .filter(|row| row.folder)
            .map(|row| row.path.clone())
            .collect();
        for folder in folders {
            self.tree.set_expanded(&folder, true);
        }
        self.read_pending(cx);
        cx.notify();
    }

    pub fn collapse_all_quiet(&mut self, cx: &mut Context<Self>) {
        self.tree.collapse_all();
        self.read_pending(cx);
        cx.notify();
    }

    pub fn expand_node_quiet(&mut self, cx: &mut Context<Self>) {
        let Some(index) = self.selected_index() else {
            return;
        };
        let row = &self.tree.rows()[index];
        if row.folder && !row.open {
            let path = row.path.clone();
            self.tree.toggle(&path);
        } else if row.folder && index + 1 < self.tree.rows().len() {
            let path = self.tree.rows()[index + 1].path.clone();
            if let Some(next) = self.tree.index_of(&path) {
                self.selection = vec![path];
                self.anchor = Some(next);
            }
        }
        self.read_pending(cx);
        cx.notify();
    }

    pub fn collapse_node_quiet(&mut self, cx: &mut Context<Self>) {
        let Some(index) = self.selected_index() else {
            return;
        };
        let row = &self.tree.rows()[index];
        if row.folder && row.open {
            let path = row.path.clone();
            self.tree.toggle(&path);
        } else if let Some(parent) = self.tree.parent_of(index) {
            let path = self.tree.rows()[parent].path.clone();
            self.selection = vec![path];
            self.anchor = Some(parent);
        }
        self.read_pending(cx);
        cx.notify();
    }

    /// What the window saves after a quiet tree command.
    pub fn tree_state(&self) -> (Vec<PathBuf>, Option<PathBuf>) {
        (
            self.tree.expanded(),
            self.selected_path().map(Path::to_path_buf),
        )
    }

    /// The rows a menu acts on: the selection when the clicked row is in it, else that row.
    fn paths_for(&self, index: usize) -> Vec<PathBuf> {
        let Some(row) = self.tree.rows().get(index) else {
            return Vec::new();
        };
        if self.selection.contains(&row.path) {
            self.selected_paths()
        } else {
            vec![row.path.clone()]
        }
    }

    fn copy_rows(&mut self, cut: bool, cx: &mut Context<Self>) {
        let paths = self
            .selected_paths()
            .into_iter()
            .filter(|path| path != &self.root_path())
            .collect::<Vec<_>>();
        if paths.is_empty() {
            return;
        }
        let clipboard = cx.default_global::<TreeClipboard>();
        clipboard.paths = paths;
        clipboard.cut = cut;
        cx.notify();
    }

    /// Ctrl+V: files copied or cut in the tree go into the selected folder. A file whose
    /// language names a declaration goes through the Copy Class window, which also
    /// chooses the package or folder and the new name.
    fn paste_rows(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let clipboard = cx.default_global::<TreeClipboard>();
        let (paths, cut) = (clipboard.paths.clone(), clipboard.cut);
        if paths.is_empty() {
            return;
        }
        let selected = self.selected_row_path();
        let target = target_folder(
            &self.root_path(),
            selected
                .as_ref()
                .map(|(path, folder)| (path.as_path(), *folder)),
        );
        if let [source] = paths.as_slice()
            && source.is_file()
            && !(cut && source.parent() == Some(target.as_path()))
            && let Some(stem) = self.class_stem(source, cx)
        {
            let default_name = crate::copy_class::default_name(&target, &stem, source.extension());
            let panel = cx.weak_entity();
            let handle = window.window_handle();
            let subject = source.clone();
            let submit = move |name: String, folder: PathBuf, cx: &mut App| {
                let (source, panel) = (subject.clone(), panel.clone());
                cx.defer(move |cx| {
                    let run = move |_: AnyView, window: &mut Window, cx: &mut App| {
                        panel
                            .update(cx, |this, cx| {
                                this.copy_class_done(
                                    source.clone(),
                                    name.clone(),
                                    folder.clone(),
                                    cut,
                                    window,
                                    cx,
                                );
                            })
                            .ok();
                    };
                    handle.update(cx, run).ok();
                });
            };
            let show = |workspace: &mut Workspace, cx: &mut Context<Workspace>| {
                crate::copy_class::CopyClassDialog::show(
                    source,
                    &target,
                    &default_name,
                    cut,
                    submit,
                    workspace,
                    window,
                    cx,
                );
            };
            self.workspace.update(cx, show).ok();
            return;
        }
        // A cut beside the source does nothing: the clipboard is kept as it is.
        let acts = paths
            .iter()
            .any(|from| from.parent() != Some(target.as_path()));
        if cut && acts {
            cx.default_global::<TreeClipboard>().paths.clear();
        }
        // Names and the existence of the places are checked in the background: the
        // window thread does not touch the disk.
        let sources = paths.clone();
        let task = cx.background_executor().spawn(async move {
            let mut moves = Vec::new();
            for from in &sources {
                match plan_paste(from, &target, cut) {
                    Ok(Some(to)) => moves.push((from.clone(), to)),
                    Ok(None) => {}
                    Err(error) => return Err((from.clone(), error)),
                }
            }
            for (from, to) in &moves {
                let result = if cut {
                    rename_on_disk(from, to)
                } else {
                    copy_path(from, to)
                };
                if let Err(error) = result {
                    return Err((from.clone(), error.to_string()));
                }
            }
            Ok(moves)
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            this.update_in(cx, |this, window, cx| match result {
                Ok(moves) => {
                    for (from, to) in &moves {
                        if cut {
                            let follow =
                                |workspace: &mut Workspace, cx: &mut Context<Workspace>| {
                                    workspace.paths_moved(from, to, window, cx)
                                };
                            this.workspace.update(cx, follow).ok();
                            this.tree.moved(from, to);
                        }
                    }
                    if let Some((_, to)) = moves.last() {
                        this.select_path(to, cx);
                    }
                    this.reload(cx);
                }
                Err((from, error)) => {
                    let text = format!("{}: {error}", from.display());
                    this.report("Cannot paste", text, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// The stem of `source` for the Copy Class window, when its language names the
    /// declaration. The buffer is the text when it is open and has unsaved edits.
    fn class_stem(&self, source: &Path, cx: &mut Context<Self>) -> Option<String> {
        let stem = source.file_stem()?.to_str()?.to_owned();
        let support = cx.try_global::<Semantic>()?;
        let text = self
            .workspace
            .read_with(cx, |workspace, app| {
                crate::copy_class::source_text(workspace, source, app)
            })
            .ok()
            .flatten()?;
        crate::copy_class::knows_declaration(Some(support), source, &text, &stem).then_some(stem)
    }

    /// The Copy Class window is done: a copy is written, a cut goes to the language to
    /// move it with its references or, without a server, as a file with its package.
    fn copy_class_done(
        &mut self,
        source: PathBuf,
        name: String,
        folder: PathBuf,
        cut: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if cut {
            cx.default_global::<TreeClipboard>().paths.clear();
            self.cut_class(source, name, folder, window, cx);
            return;
        }
        let result = self
            .workspace
            .read_with(cx, |workspace, app| {
                crate::copy_class::copy_file(workspace, &source, &name, &folder, app)
            })
            .unwrap_or_else(|_| Err("The window is gone".to_owned()));
        match result {
            Ok(target) => {
                self.select_path(&target, cx);
                self.reload(cx);
                self.open_file(target, window, cx);
            }
            Err(error) => self.report("Cannot copy", error, cx),
        }
    }

    fn cut_class(
        &mut self,
        source: PathBuf,
        name: String,
        folder: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if source.parent() == Some(folder.as_path()) {
            return;
        }
        let stem = source
            .file_stem()
            .map_or_else(String::new, |stem| stem.to_string_lossy().into_owned());
        let support = cx
            .try_global::<Semantic>()
            .map(|semantic| semantic.0.clone());
        let can_move = name == stem
            && support.as_ref().is_some_and(|support| {
                support.matches(&source)
                    && support.move_destinations().is_some()
                    && support.move_file().is_some()
            });
        if can_move {
            let to = folder.join(crate::copy_class::file_name(&source, &name));
            self.move_file(source, to, window, cx);
            return;
        }
        let result = self
            .workspace
            .read_with(cx, |workspace, app| {
                crate::copy_class::move_file(workspace, &source, &name, &folder, app)
            })
            .unwrap_or_else(|_| Err("The window is gone".to_owned()));
        match result {
            Ok(target) => {
                let close = |workspace: &mut Workspace, cx: &mut Context<Workspace>| {
                    workspace.discard_tabs_under(&source, window, cx)
                };
                self.workspace.update(cx, close).ok();
                self.select_path(&target, cx);
                self.reload(cx);
                self.open_file(target, window, cx);
            }
            Err(error) => self.report("Cannot move", error, cx),
        }
    }

    fn copy_paths(&mut self, cx: &mut Context<Self>) {
        let paths = self.selected_paths();
        if paths.is_empty() {
            return;
        }
        let text = paths
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
            .join("\n");
        cx.write_to_clipboard(ClipboardItem::new_string(text));
    }

    /// Ctrl+NumPad Minus: every folder but the root closes.
    fn collapse_all(&mut self, cx: &mut Context<Self>) {
        self.tree.collapse_all();
        if self.selected_index().is_none() {
            let root = self.root_path();
            self.selection = vec![root];
            self.anchor = Some(0);
        }
        self.state_changed(cx);
    }

    /// The speed search of the tree: typed letters pick the next row that starts with
    /// them, and Backspace takes the last letter back.
    fn search_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) -> bool {
        let key = &event.keystroke;
        if key.modifiers.control || key.modifiers.alt || key.modifiers.platform {
            return false;
        }
        let now = Instant::now();
        let expired = self
            .search_at
            .is_none_or(|at| now.duration_since(at) > SEARCH_TIMEOUT);
        if key.key == "backspace" {
            if self.search.is_empty() {
                return false;
            }
            self.search.pop();
        } else {
            let Some(text) = &key.key_char else {
                return false;
            };
            if text.chars().count() != 1 || text.chars().any(char::is_control) {
                return false;
            }
            if expired {
                self.search.clear();
            }
            self.search.push_str(text);
        }
        self.search_at = Some(now);
        if self.search.is_empty() {
            return true;
        }
        let needle = self.search.to_lowercase();
        let count = self.tree.rows().len();
        let start = if expired {
            0
        } else {
            self.selected_index().map_or(0, |index| index + 1)
        };
        let found = (0..count).map(|step| (start + step) % count).find(|index| {
            self.tree.rows()[*index]
                .name
                .to_lowercase()
                .starts_with(&needle)
        });
        if let Some(index) = found {
            self.select(index, cx);
        }
        true
    }

    fn key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let key = &event.keystroke;
        let plain_move = !key.modifiers.control && !key.modifiers.alt && !key.modifiers.shift;
        if plain_move && key.key == "-" {
            cx.stop_propagation();
            self.collapse_all(cx);
            return;
        }
        if key.modifiers.shift
            && !key.modifiers.control
            && !key.modifiers.alt
            && matches!(key.key.as_str(), "up" | "down")
        {
            cx.stop_propagation();
            let delta = if key.key == "up" { -1 } else { 1 };
            self.extend_selection_by(delta, cx);
            return;
        }
        if self.search_key(event, cx) {
            cx.stop_propagation();
        }
    }

    fn expand_or_descend(&mut self, cx: &mut Context<Self>) {
        let Some(index) = self.selected_index() else {
            return;
        };
        let row = &self.tree.rows()[index];
        if row.folder && !row.open {
            let path = row.path.clone();
            self.toggle(&path, cx);
        } else if row.folder {
            self.select(index + 1, cx);
        }
    }

    fn collapse_or_ascend(&mut self, cx: &mut Context<Self>) {
        let Some(index) = self.selected_index() else {
            return;
        };
        let row = &self.tree.rows()[index];
        if row.folder && row.open {
            let path = row.path.clone();
            self.toggle(&path, cx);
        } else if let Some(parent) = self.tree.parent_of(index) {
            self.select(parent, cx);
        }
    }

    fn activate_row(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(row) = self.tree.rows().get(index) else {
            return;
        };
        let path = row.path.clone();
        if row.folder {
            self.toggle(&path, cx);
        } else {
            self.open_file(path, window, cx);
        }
    }

    pub fn open_file(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.workspace
            .update(cx, |workspace, cx| workspace.open_file(path, window, cx))
            .ok();
    }

    fn click(
        &mut self,
        index: usize,
        event: &ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus, cx);
        let modifiers = event.modifiers();
        if modifiers.control {
            self.toggle_selection(index, cx);
        } else if modifiers.shift {
            self.extend_selection(index, cx);
        } else {
            self.select(index, cx);
        }
        let folder = self.tree.rows().get(index).is_some_and(|row| row.folder);
        if !modifiers.control && !modifiers.shift && (folder || event.click_count() >= 2) {
            self.activate_row(index, window, cx);
        }
    }

    /// Shows `path` in the tree: its folders open, the row is selected and in view.
    pub fn select_path(&mut self, path: &Path, cx: &mut Context<Self>) {
        if let Some(folder) = path.parent() {
            self.reveal(folder, cx);
        }
        match self.tree.index_of(path) {
            Some(index) => self.select(index, cx),
            None => {
                self.selection = vec![path.to_path_buf()];
                self.anchor = None;
                self.pending_select = Some(path.to_path_buf());
            }
        }
    }

    fn root_path(&self) -> PathBuf {
        self.tree.rows()[0].path.clone()
    }

    fn report(&mut self, title: &str, text: String, cx: &mut Context<Self>) {
        let title = title.to_string();
        self.workspace
            .update(cx, |workspace, cx| workspace.notify_user(title, text, cx))
            .ok();
    }

    /// Runs `next` on the panel once the window is free: the popup that asked is still
    /// being handled, and the window cannot be updated twice.
    fn later(
        window: &Window,
        panel: WeakEntity<Self>,
        cx: &mut App,
        next: impl FnOnce(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) {
        let handle = window.window_handle();
        cx.defer(move |cx| {
            let run = move |_: AnyView, window: &mut Window, cx: &mut App| {
                panel.update(cx, |this, cx| next(this, window, cx)).ok();
            };
            handle.update(cx, run).ok();
        });
    }

    /// Asks for the name of something new in `folder`.
    pub fn ask_new(
        &mut self,
        template: FileTemplate,
        folder: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let title = format!("New {}", template.label);
        let panel = cx.weak_entity();
        let handle = window.window_handle();
        let submit = move |name: String, cx: &mut App| {
            let (template, folder) = (template.clone(), folder.clone());
            let panel = panel.clone();
            cx.defer(move |cx| {
                let run = move |_: AnyView, window: &mut Window, cx: &mut App| {
                    let create = |this: &mut Self, cx: &mut Context<Self>| {
                        this.make_item(template, folder, name, window, cx)
                    };
                    panel.update(cx, create).ok();
                };
                handle.update(cx, run).ok();
            });
        };
        let ask = |workspace: &mut Workspace, cx: &mut Context<Workspace>| {
            Prompt::show(&title, "Name", submit, workspace, window, cx)
        };
        self.workspace.update(cx, ask).ok();
    }

    /// A script step: creates `name` from the template with that label in `folder`.
    pub(crate) fn script_new(
        &mut self,
        label: &str,
        folder: &Path,
        name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let known = self
            .templates
            .iter()
            .find(|template| template.label == label);
        match known.cloned() {
            Some(template) => {
                let folder = self.root_path().join(folder);
                self.make_item(template, folder, name, window, cx)
            }
            None => self.report("Script", format!("no template {label}"), cx),
        }
    }

    pub(crate) fn script_rename(
        &mut self,
        path: &Path,
        name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let from = self.root_path().join(path);
        self.rename(from, name, window, cx);
    }

    pub(crate) fn script_delete(
        &mut self,
        path: &Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let path = self.root_path().join(path);
        self.delete(path, window, cx);
    }

    fn make_item(
        &mut self,
        template: FileTemplate,
        folder: PathBuf,
        input: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let definition = crate::settings_templates::file_template(cx, template.label);
        let plan = match definition.as_ref() {
            Some(definition) => plan_creation_with_template(
                &template,
                &folder,
                &self.root_path(),
                &input,
                Some(definition),
            ),
            None => crate::tree_ops::plan_creation(&template, &folder, &self.root_path(), &input),
        };
        let plan = match plan {
            Ok(plan) => plan,
            Err(error) => return self.report("Cannot create", error, cx),
        };
        let write = cx
            .background_executor()
            .spawn(async move { create_on_disk(&plan).map(|()| plan) });
        cx.spawn_in(window, async move |this, cx| match write.await {
            Ok(plan) => {
                let shown = |this: &mut Self, window: &mut Window, cx: &mut Context<Self>| {
                    this.created(plan, window, cx)
                };
                this.update_in(cx, shown).ok();
            }
            Err(error) => {
                let text = format!("{input}: {error}");
                this.update(cx, |this, cx| this.report("Cannot create", text, cx))
                    .ok();
            }
        })
        .detach();
    }

    fn created(&mut self, made: Creation, window: &mut Window, cx: &mut Context<Self>) {
        self.select_path(&made.path, cx);
        self.reload(cx);
        if made.content.is_some() {
            self.open_file(made.path, window, cx);
        }
    }

    fn ask_rename(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        if path == self.root_path() {
            return;
        }
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        let name = name.into_owned();
        let panel = cx.weak_entity();
        let handle = window.window_handle();
        let from = path.clone();
        let submit = move |new_name: String, cx: &mut App| {
            let (from, panel) = (from.clone(), panel.clone());
            cx.defer(move |cx| {
                let run = move |_: AnyView, window: &mut Window, cx: &mut App| {
                    let rename = |this: &mut Self, cx: &mut Context<Self>| {
                        this.rename(from, new_name, window, cx)
                    };
                    panel.update(cx, rename).ok();
                };
                handle.update(cx, run).ok();
            });
        };
        let ask = |workspace: &mut Workspace, cx: &mut Context<Workspace>| {
            Prompt::show_with("Rename", &name, submit, workspace, window, cx)
        };
        self.workspace.update(cx, ask).ok();
    }

    fn rename(
        &mut self,
        from: PathBuf,
        new_name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let to = match renamed(&from, &new_name) {
            Ok(to) => to,
            Err(error) => return self.report("Cannot rename", error, cx),
        };
        if to == from {
            return;
        }
        self.move_on_disk(from, to, "Cannot rename", window, cx);
    }

    /// Moves a file or a folder on disk and takes its tabs and rows along. Unsaved text
    /// goes to the old place first.
    fn move_on_disk(
        &mut self,
        from: PathBuf,
        to: PathBuf,
        title: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let save = |workspace: &mut Workspace, cx: &mut Context<Workspace>| {
            workspace.save_tabs_under(&from, cx)
        };
        self.workspace.update(cx, save).ok();
        let (old, new) = (from.clone(), to.clone());
        let history = crate::history::get(cx);
        let task = cx.background_executor().spawn(async move {
            if let Some(history) = &history {
                let _ = history.record_tree(&old, local_history::Cause::BeforeRename);
            }
            rename_on_disk(&old, &new)
        });
        cx.spawn_in(window, async move |this, cx| match task.await {
            Ok(()) => {
                let moved = |this: &mut Self, window: &mut Window, cx: &mut Context<Self>| {
                    this.renamed(from, to, window, cx)
                };
                this.update_in(cx, moved).ok();
            }
            Err(error) => {
                let text = format!("{}: {error}", from.display());
                this.update(cx, |this, cx| this.report(title, text, cx))
                    .ok();
            }
        })
        .detach();
    }

    /// A row was dropped on another one: a file goes into that folder, a folder too.
    fn drop_on(
        &mut self,
        target: PathBuf,
        target_is_folder: bool,
        dragged: DraggedTree,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let target_folder = if target_is_folder {
            target
        } else {
            target
                .parent()
                .map_or_else(|| self.root_path(), Path::to_path_buf)
        };
        let to = match plan_move(&dragged.path, &target_folder, dragged.folder) {
            Ok(Some(to)) => to,
            Ok(None) => return,
            Err(error) => return self.report("Cannot move", error, cx),
        };
        if dragged.folder {
            self.move_on_disk(dragged.path, to, "Cannot move", window, cx);
        } else {
            self.move_file(dragged.path, to, window, cx);
        }
    }

    /// A file dropped into another folder: the language moves the class and updates the
    /// references when it can; a file without one moves as it is.
    fn move_file(
        &mut self,
        from: PathBuf,
        to: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let target_folder = to
            .parent()
            .map_or_else(|| self.root_path(), Path::to_path_buf);
        let support = cx
            .try_global::<Semantic>()
            .map(|semantic| semantic.0.clone());
        let can_move = support.as_ref().is_some_and(|support| {
            support.matches(&from)
                && support.move_destinations().is_some()
                && support.move_file().is_some()
        });
        if !can_move {
            return self.move_on_disk(from, to, "Cannot move", window, cx);
        }
        // The server moves a document it has open, so the file is shown first.
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.open_file(from.clone(), window, cx)
            })
            .ok();
        let view = self
            .workspace
            .read_with(cx, |workspace, _| workspace.view_of(&from))
            .ok()
            .flatten()
            .and_then(|view| view.downcast::<EditorView>().ok());
        let Some(view) = view else {
            return self.move_on_disk(from, to, "Cannot move", window, cx);
        };
        // The file loads in the background: wait for the event, not for a clock. A
        // tab that closes sends the same event, so the wait ends with it.
        let (events, mut loaded) = futures_channel::mpsc::unbounded();
        let listen = cx.subscribe(&view, {
            let events = events.clone();
            move |_, _, event: &EditorEvent, _| {
                if matches!(event, EditorEvent::Loaded | EditorEvent::Failed(_)) {
                    let _ = events.unbounded_send(());
                }
            }
        });
        let closed = cx.observe_release(&view, {
            let events = events.clone();
            move |_, _, _| {
                let _ = events.unbounded_send(());
            }
        });
        let weak = view.downgrade();
        drop(events);
        // The release listener only runs when no strong handle is left: the wait holds
        // the view weakly, so a closed tab ends it.
        drop(view);
        let panel = cx.weak_entity();
        let handle = window.window_handle();
        cx.spawn(async move |_, cx| {
            // The subscriptions live until the wait is over.
            let _listen = listen;
            let _closed = closed;
            let loaded_now = weak
                .read_with(cx, |view, _| view.text().is_some())
                .unwrap_or(false);
            if !loaded_now {
                let _ = loaded.next().await;
            }
            let Some(view) = weak.upgrade() else {
                return;
            };
            let is_loaded = view.read_with(cx, |view, _| view.text().is_some());
            if !is_loaded {
                // The tab failed to load or was closed: the window has said why.
                return;
            }
            let outcome = view.update(cx, |view, cx| view.move_to_folder(&target_folder, cx));
            let outcome = match outcome {
                Some(mut outcome) => outcome.next().await,
                None => Some(FolderMove::Plain),
            };
            let run = move |_: AnyView, window: &mut Window, cx: &mut App| {
                panel
                    .update(cx, |this, cx| match outcome {
                        Some(FolderMove::AskPlain) => this.ask_plain_move(from, to, window, cx),
                        Some(FolderMove::Handled) => {}
                        Some(FolderMove::Plain) | None => {
                            this.move_on_disk(from, to, "Cannot move", window, cx)
                        }
                    })
                    .ok();
            };
            handle.update(cx, run).ok();
        })
        .detach();
    }

    /// A class dropped on a folder that is not a package: a plain move leaves its
    /// references behind, so it asks before doing that.
    fn ask_plain_move(
        &mut self,
        from: PathBuf,
        to: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let name = from.file_name().unwrap_or_default().to_string_lossy();
        let question = format!("Move \"{name}\" without updating references?");
        let panel = cx.weak_entity();
        let handle = window.window_handle();
        let submit = move |cx: &mut App| {
            let (from, to, panel) = (from.clone(), to.clone(), panel.clone());
            cx.defer(move |cx| {
                let run = move |_: AnyView, window: &mut Window, cx: &mut App| {
                    panel
                        .update(cx, |this, cx| {
                            this.move_on_disk(from, to, "Cannot move", window, cx)
                        })
                        .ok();
                };
                handle.update(cx, run).ok();
            });
        };
        let ask = |workspace: &mut Workspace, cx: &mut Context<Workspace>| {
            Prompt::ask("Move file", &question, submit, workspace, window, cx)
        };
        self.workspace.update(cx, ask).ok();
    }

    fn renamed(&mut self, from: PathBuf, to: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let follow = |workspace: &mut Workspace, cx: &mut Context<Workspace>| {
            workspace.paths_moved(&from, &to, window, cx)
        };
        self.workspace.update(cx, follow).ok();
        self.tree.moved(&from, &to);
        self.select_path(&to, cx);
        self.reload(cx);
    }

    /// A refactoring moved a file or a folder: its rows follow it.
    pub fn paths_moved(&mut self, from: &Path, to: &Path, cx: &mut Context<Self>) {
        self.tree.moved(from, to);
        self.select_path(to, cx);
        self.reload(cx);
    }

    /// Delete on a multiple selection asks once and takes every selected row away.
    fn ask_delete_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths = self
            .selected_paths()
            .into_iter()
            .filter(|path| path != &self.root_path())
            .collect::<Vec<_>>();
        match paths.len() {
            0 => {}
            1 => self.ask_delete(paths[0].clone(), window, cx),
            count => {
                let question = format!("Move {count} items to the Recycle Bin?");
                let panel = cx.weak_entity();
                let handle = window.window_handle();
                let submit = move |cx: &mut App| {
                    let (paths, panel) = (paths.clone(), panel.clone());
                    cx.defer(move |cx| {
                        let run = move |_: AnyView, window: &mut Window, cx: &mut App| {
                            panel
                                .update(cx, |this, cx| this.delete_paths(paths, window, cx))
                                .ok();
                        };
                        handle.update(cx, run).ok();
                    });
                };
                let ask = |workspace: &mut Workspace, cx: &mut Context<Workspace>| {
                    Prompt::ask("Delete", &question, submit, workspace, window, cx)
                };
                self.workspace.update(cx, ask).ok();
            }
        }
    }

    /// Deletes every path in the background, then lets the tree forget them.
    fn delete_paths(&mut self, paths: Vec<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        let history = crate::history::get(cx);
        let targets = paths.clone();
        let task = cx.background_executor().spawn(async move {
            for target in &targets {
                if let Some(history) = &history {
                    let _ = history.record_tree(target, local_history::Cause::BeforeDelete);
                }
                delete_on_disk(target)?;
                if let Some(history) = &history {
                    let _ = history.record_removed(target);
                }
            }
            Ok::<(), std::io::Error>(())
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            this.update_in(cx, |this, window, cx| match result {
                Ok(()) => {
                    for path in &paths {
                        let close = |workspace: &mut Workspace, cx: &mut Context<Workspace>| {
                            workspace.discard_tabs_under(path, window, cx)
                        };
                        this.workspace.update(cx, close).ok();
                        this.tree.forget(path);
                    }
                    this.selection.clear();
                    this.anchor = None;
                    this.state_changed(cx);
                    this.reload(cx);
                }
                Err(error) => {
                    let text = error.to_string();
                    this.report("Cannot delete", text, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    fn ask_delete(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        if path == self.root_path() {
            return;
        }
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        let question = format!("Move \"{name}\" to the Recycle Bin?");
        let panel = cx.weak_entity();
        let handle = window.window_handle();
        let submit = move |cx: &mut App| {
            let (path, panel) = (path.clone(), panel.clone());
            cx.defer(move |cx| {
                let run = move |_: AnyView, window: &mut Window, cx: &mut App| {
                    let delete =
                        |this: &mut Self, cx: &mut Context<Self>| this.delete(path, window, cx);
                    panel.update(cx, delete).ok();
                };
                handle.update(cx, run).ok();
            });
        };
        let ask = |workspace: &mut Workspace, cx: &mut Context<Workspace>| {
            Prompt::ask("Delete", &question, submit, workspace, window, cx)
        };
        self.workspace.update(cx, ask).ok();
    }

    /// Alt+Delete: opens the file and lets the editor check its usages before the
    /// type alone in it is deleted. A file without a language keeps the prompt.
    fn safe_delete(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let offset = cx
            .try_global::<Semantic>()
            .and_then(|semantic| {
                std::fs::read_to_string(&path)
                    .ok()
                    .map(|text| (text, semantic))
            })
            .and_then(|(text, semantic)| semantic.0.syntax().first_type_offset(&text));
        let Some(offset) = offset else {
            return self.ask_delete(path, window, cx);
        };
        cx.default_global::<crate::editor_tab::PendingSafeDelete>()
            .0 = Some((path.clone(), offset));
        let view = self.workspace.update(cx, |workspace, cx| {
            workspace.open_file(path.clone(), window, cx);
            workspace
                .view_of(&path)
                .and_then(|view| view.downcast::<EditorView>().ok())
        });
        if let Ok(Some(view)) = view {
            let loaded = view.read_with(cx, |view, _| view.is_loaded());
            if loaded {
                cx.default_global::<crate::editor_tab::PendingSafeDelete>()
                    .0 = None;
                view.update(cx, |view, cx| view.safe_delete_at_offset(offset, cx));
            }
        }
    }

    fn delete(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        // The file goes first: if that fails, a file in use for one, the tabs and their
        // unsaved text stay as they are.
        let target = path.clone();
        let history = crate::history::get(cx);
        let task = cx.background_executor().spawn(async move {
            if let Some(history) = &history {
                let _ = history.record_tree(&target, local_history::Cause::BeforeDelete);
            }
            delete_on_disk(&target)?;
            if let Some(history) = &history {
                let _ = history.record_removed(&target);
            }
            Ok::<(), std::io::Error>(())
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let finish = |this: &mut Self, window: &mut Window, cx: &mut Context<Self>| match result
            {
                Ok(()) => this.deleted(&path, window, cx),
                Err(error) => {
                    let text = format!("{}: {error}", path.display());
                    this.report("Cannot delete", text, cx);
                }
            };
            this.update_in(cx, finish).ok();
        })
        .detach();
    }

    /// The file is gone: its tabs close without being written back, which would bring it back.
    fn deleted(&mut self, path: &Path, window: &mut Window, cx: &mut Context<Self>) {
        let close = |workspace: &mut Workspace, cx: &mut Context<Workspace>| {
            workspace.discard_tabs_under(path, window, cx)
        };
        self.workspace.update(cx, close).ok();
        self.tree.forget(path);
        self.selection = path.parent().map(Path::to_path_buf).into_iter().collect();
        self.anchor = None;
        self.state_changed(cx);
        self.reload(cx);
    }

    fn selected_row_path(&self) -> Option<(PathBuf, bool)> {
        let row = self.tree.rows().get(self.selected_index()?)?;
        Some((row.path.clone(), row.folder))
    }

    /// Alt+Insert: the New menu for the selection, at the top of the tree.
    fn show_new_menu(&mut self, cx: &mut Context<Self>) {
        let selected = self.selected_row_path();
        let folder = target_folder(
            &self.root_path(),
            selected
                .as_ref()
                .map(|(path, folder)| (path.as_path(), *folder)),
        );
        let entries = self.new_entries(folder, cx);
        let position = point(px(160.), px(120.));
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.show_menu(position, entries, cx)
            })
            .ok();
    }

    fn new_entries(&self, folder: PathBuf, cx: &mut Context<Self>) -> Vec<MenuEntry> {
        let panel = cx.weak_entity();
        let templates = self.templates.iter().cloned();
        templates
            .map(|template| {
                let (panel, folder) = (panel.clone(), folder.clone());
                let (label, icon) = (format!("New {}", template.label), template.icon);
                MenuEntry::new(label, move |window, cx| {
                    let (template, folder) = (template.clone(), folder.clone());
                    let ask = |this: &mut Self, window: &mut Window, cx: &mut Context<Self>| {
                        this.ask_new(template, folder, window, cx)
                    };
                    Self::later(window, panel.clone(), cx, ask);
                })
                .icon(icon)
            })
            .collect()
    }

    /// The Menu key in the tree: the rows of the context menu of the selection.
    pub fn context_menu_entries(
        &mut self,
        cx: &mut Context<Self>,
    ) -> (Point<Pixels>, Vec<MenuEntry>) {
        let index = self.selected_index().unwrap_or(0);
        self.menu_entries(index, point(px(180.), px(200.)), cx)
    }

    fn show_menu(&mut self, index: usize, position: Point<Pixels>, cx: &mut Context<Self>) {
        let (position, entries) = self.menu_entries(index, position, cx);
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.show_menu(position, entries, cx)
            })
            .ok();
    }

    fn menu_entries(
        &mut self,
        index: usize,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) -> (Point<Pixels>, Vec<MenuEntry>) {
        let Some(row) = self.tree.rows().get(index) else {
            return (position, Vec::new());
        };
        let (path, is_folder, is_root) = (row.path.clone(), row.folder, index == 0);
        if !self.selection.contains(&path) {
            // The caller is already updating the window, so only the selection changes.
            self.selection = vec![path.clone()];
            self.anchor = Some(index);
        }
        let selected = self.paths_for(index);
        let text = selected
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
            .join("\n");
        let panel = cx.weak_entity();
        let folder = target_folder(&self.root_path(), Some((path.as_path(), is_folder)));
        let mut entries = self.new_entries(folder, cx);
        entries.push(MenuEntry::separator());
        if !is_folder {
            let (panel, path) = (panel.clone(), path.clone());
            let open = MenuEntry::new("Open", move |window, cx| {
                let path = path.clone();
                let open = |this: &mut Self, window: &mut Window, cx: &mut Context<Self>| {
                    this.open_file(path, window, cx)
                };
                Self::later(window, panel.clone(), cx, open);
            });
            entries.push(open.shortcut("F4"));
        }
        if !is_root {
            let (rename_panel, rename_path) = (panel.clone(), path.clone());
            let rename = MenuEntry::new("Rename...", move |window, cx| {
                let path = rename_path.clone();
                let ask = |this: &mut Self, window: &mut Window, cx: &mut Context<Self>| {
                    this.ask_rename(path, window, cx)
                };
                Self::later(window, rename_panel.clone(), cx, ask);
            });
            entries.push(rename.shortcut("Shift+F6"));
            let delete_panel = panel;
            let delete = MenuEntry::new("Delete...", move |window, cx| {
                let ask = |this: &mut Self, window: &mut Window, cx: &mut Context<Self>| {
                    this.ask_delete_selection(window, cx)
                };
                Self::later(window, delete_panel.clone(), cx, ask);
            });
            entries.push(delete.shortcut("Delete"));
            entries.push(MenuEntry::separator());
        }
        let (history_panel, history_path) = (cx.weak_entity(), path.clone());
        let history = MenuEntry::new("Local History...", move |window, cx| {
            let path = history_path.clone();
            let show = |this: &mut Self, window: &mut Window, cx: &mut Context<Self>| {
                let open = |workspace: &mut Workspace, cx: &mut Context<Workspace>| {
                    crate::history_view::HistoryView::show(path, workspace, window, cx)
                };
                this.workspace.update(cx, open).ok();
            };
            Self::later(window, history_panel.clone(), cx, show);
        });
        entries.push(history);
        entries.push(MenuEntry::separator());
        let copy = MenuEntry::new("Copy Path", move |_, cx| {
            cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
        });
        entries.push(copy.icon("general/copy"));
        let reveal = path.clone();
        entries.push(MenuEntry::new("Reveal in Explorer", move |_, cx| {
            ui_kit::reveal_path(cx, &reveal)
        }));
        if !is_folder {
            let (log_panel, log_path) = (cx.weak_entity(), path.clone());
            let show = MenuEntry::new("Show History", move |window, cx| {
                let path = log_path.clone();
                let open = |this: &mut Self, window: &mut Window, cx: &mut Context<Self>| {
                    let open = |workspace: &mut Workspace, cx: &mut Context<Workspace>| {
                        crate::git_log::show_file(path, workspace, window, cx)
                    };
                    this.workspace.update(cx, open).ok();
                };
                Self::later(window, log_panel.clone(), cx, open);
            });
            entries.push(show);
        }
        (position, entries)
    }

    fn row_icon(&self, index: usize) -> &'static str {
        let row = &self.tree.rows()[index];
        match (index, row.folder) {
            (0, _) => ROOT_ICON,
            (_, true) => FOLDER_ICON,
            (_, false) => self.icons.icon_for(&row.path),
        }
    }

    fn rows(&self, range: Range<usize>, cx: &mut Context<Self>) -> Vec<Div> {
        let theme = *cx.global::<Theme>();
        range
            .map(|index| {
                let row = &self.tree.rows()[index];
                let selected = self.selection.contains(&row.path);
                let name = row.name.clone();
                let style = TreeRow {
                    id: ("tree", index).into(),
                    depth: row.depth,
                    label: row.name.clone().into(),
                    hint: (index == 0).then(|| row.path.display().to_string().into()),
                    icon: self.row_icon(index),
                    expandable: row.folder,
                    open: row.open,
                    selected,
                };
                let tint = cx
                    .try_global::<crate::git::ActiveGit>()
                    .and_then(|git| git.0.read(cx).kind_of(&row.path));
                let path = row.path.clone();
                let folder = row.folder;
                let dragged = DraggedTree {
                    path: path.clone(),
                    folder,
                    name: row.name.clone().into(),
                };
                let target = path.clone();
                let row = tree_row(style, &theme)
                    .debug_selector(move || format!("tree-row-{name}"))
                    .when_some(tint, |this, kind| this.text_color(kind.color(theme.dark)))
                    .on_click(cx.listener(move |this, event, window, cx| {
                        this.click(index, event, window, cx)
                    }))
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                            this.show_menu(index, event.position, cx)
                        }),
                    )
                    .on_drag(dragged, |dragged, _, _, cx| {
                        let name = dragged.name.clone();
                        cx.new(|_| TreeDragPreview(name))
                    })
                    .drag_over::<DraggedTree>(move |style, _, _, _| style.bg(theme.selection))
                    .on_drop(cx.listener(move |this, dragged: &DraggedTree, window, cx| {
                        this.drop_on(target.clone(), folder, dragged.clone(), window, cx)
                    }));
                div().w_full().px(px(ROW_MARGIN)).child(row)
            })
            .collect()
    }
}

impl Render for ProjectPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let count = self.tree.rows().len();
        let workspace = self.workspace.clone();
        let hide = move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
            workspace
                .update(cx, |workspace, cx| {
                    workspace.toggle_panel(DockSide::Left, window, cx)
                })
                .ok();
        };
        island(&theme)
            .id("project-panel")
            .key_context(CONTEXT)
            .track_focus(&self.focus)
            .group("left-panel")
            .h_full()
            .on_action(cx.listener(|this, _: &SelectPrevious, _, cx| this.move_selection(-1, cx)))
            .on_action(cx.listener(|this, _: &SelectNext, _, cx| this.move_selection(1, cx)))
            .on_action(cx.listener(|this, _: &ExpandRow, _, cx| this.expand_or_descend(cx)))
            .on_action(cx.listener(|this, _: &CollapseRow, _, cx| this.collapse_or_ascend(cx)))
            .on_action(cx.listener(|this, _: &RenameRow, window, cx| {
                if let Some((path, _)) = this.selected_row_path() {
                    this.ask_rename(path, window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &DeleteRow, window, cx| {
                this.ask_delete_selection(window, cx);
            }))
            .on_action(cx.listener(|this, _: &SafeDeleteRow, window, cx| {
                if let Some((path, folder)) = this.selected_row_path() {
                    if folder {
                        this.ask_delete(path, window, cx);
                    } else {
                        this.safe_delete(path, window, cx);
                    }
                }
            }))
            .on_action(cx.listener(|this, _: &NewItem, _, cx| this.show_new_menu(cx)))
            .on_action(cx.listener(|this, _: &CompareTwoFiles, window, cx| {
                this.compare_with_clipboard(window, cx)
            }))
            .on_action(cx.listener(|this, _: &OpenRow, window, cx| {
                if let Some(index) = this.selected_index() {
                    this.activate_row(index, window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &CutRows, _, cx| this.copy_rows(true, cx)))
            .on_action(cx.listener(|this, _: &CopyRows, _, cx| this.copy_rows(false, cx)))
            .on_action(cx.listener(|this, _: &PasteRows, window, cx| this.paste_rows(window, cx)))
            .on_action(cx.listener(|this, _: &CopyPaths, _, cx| this.copy_paths(cx)))
            .on_action(cx.listener(|this, _: &ExpandAllRows, _, cx| this.expand_all(cx)))
            .on_action(cx.listener(|this, _: &CollapseAllRows, _, cx| this.collapse_all(cx)))
            .on_action(cx.listener(|this, _: &ExpandNode, _, cx| this.expand_or_descend(cx)))
            .on_action(cx.listener(|this, _: &CollapseNode, _, cx| this.collapse_or_ascend(cx)))
            .on_action(cx.listener(|this, _: &OpenInRightSplitRow, window, cx| {
                if let Some((path, folder)) = this.selected_row_path()
                    && !folder
                {
                    this.workspace
                        .update(cx, |workspace, cx| {
                            workspace.open_file_in_split(path, false, window, cx)
                        })
                        .ok();
                }
            }))
            .on_key_down(cx.listener(|this, event, _, cx| this.key(event, cx)))
            .child(
                panel_header("Project", "left-panel", hide, &theme)
                    .debug_selector(|| "project-header".to_owned()),
            )
            .child(
                scrollable(
                    "project-tree",
                    &self.scroll,
                    div().pt(px(SPACE_S)).size_full().child(
                        uniform_list(
                            "project-tree",
                            count,
                            cx.processor(|this, range, _, cx| this.rows(range, cx)),
                        )
                        .track_scroll(&self.scroll)
                        .size_full(),
                    ),
                    &theme,
                    window,
                    cx,
                )
                .flex_1()
                .min_h_0(),
            )
    }
}

//! The project tree tool window on a virtual list.

use std::collections::VecDeque;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use editor_view::{EditorEvent, EditorView, FolderMove, Semantic};
use futures_util::StreamExt;
use lang_api::MigrationModel;
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use workspace::{
    DockSide, FileIcons, PROJECT_TOOL, Settings, Workspace, checked_label, window_key,
};

use crate::file_tree::{Child, FileTree, read_folder};
use crate::prompt::{Prompt, PromptKind};
use crate::tree_ops::{
    Creation, FileTemplate, create_on_disk, delete_on_disk, plan_creation_with_template,
    plan_drop_copy, plan_move, plan_paste, rename_on_disk, renamed, target_folder,
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
        CompareTwoFiles,
        ReformatRows
    ]
);

const CONTEXT: &str = "ProjectPanel";
const FOLDER_ICON: &str = "nodes/folder";
const ROOT_ICON: &str = "nodes/module";
const ROW_MARGIN: f32 = SPACE_L;
/// A pause this long starts a new speed search rather than adding to the old one.
const SEARCH_TIMEOUT: Duration = Duration::from_millis(1000);
/// The label of the migration node and of the entry of the New menu.
pub(crate) const MIGRATIONS_LABEL: &str = "Migrations";
/// The New menu row that writes the next versioned migration of the project.
const FLYWAY_LABEL: &str = "Flyway Migration";
/// The kinds of source file that live inside the New Class dialog rather than in the New
/// list itself.
const SOURCE_KINDS: [&str; 4] = ["Interface", "Enum", "Record", "Annotation"];
/// The order the New Class dialog shows the kinds after the main one.
const SOURCE_KIND_ORDER: [&str; 4] = ["Interface", "Record", "Enum", "Annotation"];
/// The rows of the New list that are not a source kind: a plain file, a folder, a package
/// and the file that describes a package.
const NEW_FIXED: [&str; 4] = ["File", "Directory", "Package", "package-info"];
/// Expand All reads this many folder levels from the root. A folder deeper than that stays
/// closed.
pub(crate) const FULL_EXPAND_MAX_DEPTH: usize = 8;
/// Expand All reads no more folders than this, so a folder with many children cannot
/// hold the window with thousands of reads. A folder past the limit stays closed.
pub(crate) const FULL_EXPAND_MAX_FOLDERS: usize = 256;

/// A row on its way to another folder by the pointer.
#[derive(Clone)]
struct DraggedTree {
    path: PathBuf,
    folder: bool,
    name: SharedString,
}

struct TreeDragPreview(SharedString);

impl Render for TreeDragPreview {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        // Ctrl copies instead of moving: the plus shows it while the key is held.
        let copy = window.modifiers().control;
        div()
            .flex()
            .items_center()
            .gap(px(SPACE_XS))
            .h(px(24.))
            .px(px(8.))
            .rounded(px(4.))
            .border_1()
            .border_color(theme.tab_selected_border)
            .bg(theme.tab_selected)
            .text_color(theme.text)
            .when(copy, |this| this.child("+"))
            .child(self.0.clone())
    }
}

/// Whether the tree shows a chain of folders that have one subfolder each as one row.
fn compact_middle_packages(cx: &App) -> bool {
    cx.try_global::<Settings>()
        .is_none_or(|settings| settings.project_compact_middle_packages)
}

/// The file and folder masks the project tree leaves out.
fn ignored_patterns(cx: &App) -> Vec<String> {
    cx.try_global::<Settings>()
        .map(|settings| settings.ignored_files.clone())
        .unwrap_or_default()
}

/// The tree keys and, from the same rows, the table the keymap check reads.
macro_rules! panel_keys {
    ($(($keys:literal, $action:ident, $idea:expr, $mac:expr)),* $(,)?) => {
        pub fn bind_keys(cx: &mut App) {
            bind_keys_for(cx, keymap::platform());
        }

        /// Binds the keys of one platform, so that a test reads the macOS table on every
        /// runner.
        fn bind_keys_for(cx: &mut App, platform: keymap::Platform) {
            $(
                let row = workspace::KeyRow {
                    keys: $keys,
                    mac: $mac,
                    action: concat!("project_panel::", stringify!($action)),
                    context: Some(CONTEXT),
                    idea: $idea,
                };
                for key in row.keys_for(platform) {
                    cx.bind_keys([KeyBinding::new(key, $action, Some(CONTEXT))]);
                }
            )*
        }

        /// The keys of the tree, for the menu hints and the keymap check.
        pub(crate) const PANEL_KEYS: &[workspace::KeyRow] = &[
            $(workspace::KeyRow {
                keys: $keys,
                mac: $mac,
                action: concat!("project_panel::", stringify!($action)),
                context: Some(CONTEXT),
                idea: $idea,
            }),*
        ];
    };
}

panel_keys![
    ("up", SelectPrevious, None, Some("up")),
    ("down", SelectNext, None, Some("down")),
    ("right", ExpandRow, None, Some("right")),
    ("left", CollapseRow, None, Some("left")),
    ("enter", OpenRow, None, Some("enter")),
    ("f4", OpenRow, Some("EditSource"), None),
    ("shift-f6", RenameRow, Some("RenameElement"), None),
    ("delete", DeleteRow, Some("$Delete"), None),
    ("alt-delete", SafeDeleteRow, Some("SafeDelete"), None),
    ("alt-insert", NewItem, Some("NewElement"), None),
    ("ctrl-x", CutRows, Some("$Cut"), None),
    ("ctrl-c", CopyRows, Some("$Copy"), None),
    ("ctrl-v", PasteRows, Some("$Paste"), None),
    ("ctrl-shift-c", CopyPaths, Some("CopyPaths"), None),
    ("ctrl-=", ExpandAllRows, Some("ExpandAll"), None),
    ("ctrl--", CollapseAllRows, Some("CollapseAll"), None),
    ("numpad_add", ExpandNode, Some("ExpandTreeNode"), None),
    (
        "numpad_subtract",
        CollapseNode,
        Some("CollapseTreeNode"),
        None
    ),
    (
        "shift-enter",
        OpenInRightSplitRow,
        Some("OpenInRightSplit"),
        None
    ),
    ("ctrl-d", CompareTwoFiles, Some("CompareTwoFiles"), None),
    ("ctrl-alt-l", ReformatRows, Some("ReformatCode"), None),
];

/// What the tree can create, from the application and the basics.
pub struct FileTemplates(pub Vec<FileTemplate>);

impl Global for FileTemplates {}

/// The kind of source file the New Class dialog chose the last time.
#[derive(Default)]
pub struct LastNewKind(pub usize);

impl Global for LastNewKind {}

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

/// Reads the folders Expand All opens, in breadth-first order, and stops at the depth and
/// folder limits. Runs off the window thread.
fn read_tree_fully(root: &Path) -> Vec<(PathBuf, Vec<Child>)> {
    let mut listings = Vec::new();
    let mut queue = VecDeque::from([(root.to_path_buf(), 0usize)]);
    while let Some((folder, depth)) = queue.pop_front() {
        if listings.len() >= FULL_EXPAND_MAX_FOLDERS {
            break;
        }
        let Ok(children) = read_folder(&folder) else {
            continue;
        };
        if depth + 1 < FULL_EXPAND_MAX_DEPTH {
            for child in &children {
                if child.folder {
                    queue.push_back((child.path.clone(), depth + 1));
                }
            }
        }
        listings.push((folder, children));
    }
    listings
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

/// A tab became active. A tree that follows the editor selects its file, once the window is
/// free: the tab changed inside an update of the window.
pub(crate) fn follow_active_file(path: &Path, cx: &mut App) {
    let follows = cx
        .try_global::<Settings>()
        .is_some_and(|settings| settings.project_always_select_opened_file);
    if !follows {
        return;
    }
    let Some(panel) = cx
        .try_global::<ActiveProjectPanel>()
        .map(|panel| panel.0.clone())
    else {
        return;
    };
    let path = path.to_path_buf();
    cx.defer(move |cx| {
        panel.update(cx, |panel, cx| panel.follow_opened_file(&path, cx));
    });
}

/// The shortcut of a window action as a tooltip writes it, `Alt+F1`.
fn window_hint(action: &str) -> Option<String> {
    let platform = keymap::platform();
    window_key(action, platform).map(|keys| keymap::shortcut_label(keys, platform))
}

/// A button of the toolbar of the tree: an icon button with its name as a debug selector.
fn tool_button(
    id: &'static str,
    icon: &'static str,
    label: &'static str,
    shortcut: Option<SharedString>,
    theme: &Theme,
) -> Stateful<Div> {
    icon_button(id, icon, label, shortcut, theme).debug_selector(move || id.to_owned())
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
    /// The migrations of the project, which the tree shows under their own node.
    migrations: MigrationModel,
    /// Whether the tree watches the run panel, which holds the project model with the
    /// source roots. The run panel is built after the tree, so it is watched later.
    runs_watched: bool,
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
        tree.set_compact(compact_middle_packages(cx));
        tree.set_ignored(ignored_patterns(cx));
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
                migrations: MigrationModel::default(),
                runs_watched: false,
            };
            // The switch of Compact Middle Packages and the ignored masks may also be
            // changed in the settings.
            cx.observe_global::<Settings>(|this: &mut Self, cx| {
                let ignored = ignored_patterns(cx);
                if let Some(index) = cx.try_global::<crate::index::ActiveIndex>() {
                    index.0.set_ignored(ignored.clone());
                }
                let mut changed = this.tree.set_ignored(ignored);
                changed |= this.apply_compact(cx);
                if changed {
                    this.read_pending(cx);
                    cx.notify();
                }
            })
            .detach();
            panel.read_pending(cx);
            panel.load_migrations(cx);
            panel
        })
    }

    pub fn focus(&self) -> FocusHandle {
        self.focus.clone()
    }

    /// The migrations the tree shows under their node.
    #[allow(dead_code)]
    pub fn migrations(&self) -> &MigrationModel {
        &self.migrations
    }

    /// Reads the migrations of the project in the background and puts their node in the
    /// tree, so the tree keeps walking as soon as the language answers.
    fn load_migrations(&mut self, cx: &mut Context<Self>) {
        let Some(support) = cx
            .try_global::<crate::database_panel::ActiveMigrations>()
            .and_then(|active| active.0.clone())
        else {
            return;
        };
        let job = cx
            .background_executor()
            .spawn(async move { support.model() });
        cx.spawn(async move |this, cx| {
            let model = job.await;
            this.update(cx, |this, cx| this.apply_migrations(model, cx))
                .ok();
        })
        .detach();
    }

    fn apply_migrations(&mut self, model: MigrationModel, cx: &mut Context<Self>) {
        let root = self.root_path();
        let folder = self.migrations_folder();
        if model.is_empty() {
            self.tree.remove_virtual_child(&root, MIGRATIONS_LABEL);
            self.tree.clear_virtual_listing(&folder);
            self.migrations = model;
            cx.notify();
            return;
        }
        self.tree.set_virtual_child(
            &root,
            Child {
                path: folder.clone(),
                name: MIGRATIONS_LABEL.to_owned(),
                folder: true,
            },
        );
        let children: Vec<Child> = model
            .rows
            .iter()
            .filter(|row| row.depth == 0)
            .map(|row| Child {
                path: row.path.clone(),
                name: row.label.clone(),
                folder: false,
            })
            .collect();
        self.tree.set_virtual_listing(folder, children);
        self.migrations = model;
        if let Some(path) = self.pending_select.clone()
            && let Some(index) = self.tree.index_of(&path)
        {
            self.pending_select = None;
            self.select(index, cx);
        }
        cx.notify();
    }

    /// The folder the migration node stands for. It holds no file: the node lists the
    /// files of the tool wherever the configuration put them.
    fn migrations_folder(&self) -> PathBuf {
        self.root_path().join(".arro").join("migrations")
    }

    /// Asks for the description of the next versioned migration and writes it where the
    /// tool reads it.
    fn ask_flyway_migration(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(next) = self.migrations.next.clone() else {
            self.report(
                "New Migration",
                "the project has no Flyway folder".to_owned(),
                cx,
            );
            return;
        };
        let panel = cx.weak_entity();
        let handle = window.window_handle();
        let folder = next.folder;
        let submit = move |description: String, cx: &mut App| {
            let (panel, folder) = (panel.clone(), folder.clone());
            cx.defer(move |cx| {
                let run = move |_: AnyView, window: &mut Window, cx: &mut App| {
                    let (description, folder) = (description.clone(), folder.clone());
                    let create = move |this: &mut Self, cx: &mut Context<Self>| {
                        this.create_flyway_migration(&description, &folder, window, cx)
                    };
                    panel.update(cx, create).ok();
                };
                handle.update(cx, run).ok();
            });
        };
        let ask = |workspace: &mut Workspace, cx: &mut Context<Workspace>| {
            Prompt::show(
                "New Flyway Migration",
                "Description",
                submit,
                workspace,
                window,
                cx,
            )
        };
        self.workspace.update(cx, ask).ok();
    }

    /// Writes `V<next>__<description>.sql` into `folder`, names the file as the language
    /// names it and shows it in the tree.
    pub(crate) fn create_flyway_migration(
        &mut self,
        description: &str,
        folder: &Path,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(support) = cx
            .try_global::<crate::database_panel::ActiveMigrations>()
            .and_then(|active| active.0.clone())
        else {
            self.report(
                "New Migration",
                "the language of the project is not ready".to_owned(),
                cx,
            );
            return;
        };
        let description = description.to_owned();
        let folder = folder.to_path_buf();
        let job = cx.background_executor().spawn(async move {
            let name = support
                .new_migration_name(&description)
                .ok_or_else(|| "the project has no Flyway folder".to_owned())?;
            std::fs::create_dir_all(&folder).map_err(|error| error.to_string())?;
            let path = folder.join(&name);
            if path.exists() {
                return Err(format!("{} already exists", path.display()));
            }
            std::fs::write(&path, format!("-- {description}\n"))
                .map_err(|error| error.to_string())?;
            Ok::<PathBuf, String>(path)
        });
        cx.spawn(async move |this, cx| {
            let path = job.await;
            this.update(cx, |this, cx| match path {
                Ok(path) => {
                    this.load_migrations(cx);
                    this.tree.set_expanded(&this.migrations_folder(), true);
                    this.select_path(&path, cx);
                }
                Err(error) => this.report("New Migration", error, cx),
            })
            .ok();
        })
        .detach();
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

    /// Ctrl+D: two selected files against each other, one against the clipboard, and
    /// two folders in the folder compare. A single folder asks for the other one.
    fn compare_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let selected = self.selected_paths();
        let git = cx
            .try_global::<crate::git::ActiveGit>()
            .map(|git| git.0.clone());
        if let [left, right] = selected.as_slice() {
            if left.is_dir() && right.is_dir() {
                let workspace = self.workspace.clone();
                let (left, right) = (left.clone(), right.clone());
                workspace
                    .update(cx, |workspace, cx| {
                        crate::folder_compare::FolderCompareView::open(
                            left, right, workspace, window, cx,
                        )
                    })
                    .ok();
                return;
            }
            let Some(git) = git else {
                return;
            };
            if left.is_dir() || right.is_dir() {
                return;
            }
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
        if path.is_dir() {
            self.compare_with_folder(path.clone(), window, cx);
            return;
        }
        let Some(git) = git else {
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

    /// Compare With... of one folder: the other folder comes from the folder picker.
    fn compare_with_folder(&mut self, left: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: None,
        });
        let workspace = self.workspace.clone();
        window
            .spawn(cx, async move |cx| {
                let chosen = paths.await.ok().and_then(Result::ok).flatten();
                let Some(right) = chosen.and_then(|paths| paths.into_iter().next()) else {
                    return;
                };
                cx.update(|window, cx| {
                    workspace
                        .update(cx, |workspace, cx| {
                            crate::folder_compare::FolderCompareView::open(
                                left, right, workspace, window, cx,
                            )
                        })
                        .ok();
                })
                .ok();
            })
            .detach();
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
            if self.tree.is_virtual_folder(&folder) {
                continue;
            }
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
        if !self.tree.has_listing(folder) || self.tree.is_virtual_folder(folder) {
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
        let open: Vec<PathBuf> = self.tree.listed_folders();
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
            // A folder read only for the rows of a compacted chain is not open, so its
            // failure is not reported.
            Err(error) if self.tree.is_expanded(&folder) => {
                let text = format!("{}: {error}", folder.display());
                self.workspace
                    .update(cx, |workspace, cx| {
                        workspace.notify_user("Cannot read folder", text, cx)
                    })
                    .ok();
            }
            Err(_) => {}
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

    /// Expand All, NumPad * and Shift+NumPad +: every folder opens, down to
    /// [`FULL_EXPAND_MAX_DEPTH`] and no more than [`FULL_EXPAND_MAX_FOLDERS`] folders, in one
    /// go. The disk is read in the background; a folder with many children cannot block the
    /// window.
    pub fn expand_fully(&mut self, cx: &mut Context<Self>) {
        let root = self.tree.root().to_path_buf();
        let read = cx
            .background_executor()
            .spawn(async move { read_tree_fully(&root) });
        cx.spawn(async move |this, cx| {
            let listings = read.await;
            this.update(cx, |this, cx| {
                this.listings += listings.len() as u64;
                this.tree.set_expanded_listings(listings);
                this.state_changed(cx);
            })
            .ok();
        })
        .detach();
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
            self.ask_copy_class(source.clone(), target.clone(), stem, cut, window, cx);
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

    /// The Select Opened File button: the tree shows the file of the active tab.
    fn select_opened_file(&mut self, cx: &mut Context<Self>) {
        let active = self
            .workspace
            .upgrade()
            .and_then(|workspace| workspace.read(cx).active_editor());
        if let Some((path, _)) = active {
            self.select_path(&path, cx);
        }
    }

    /// The View Options button: the options of the tree, as a menu at `position`.
    fn show_view_menu(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let panel = cx.weak_entity();
        let on = self.always_select_opened_file(cx);
        let always_panel = panel.clone();
        let always_select = MenuEntry::new(
            checked_label("Always Select Opened File", on),
            move |window, cx| {
                let toggle = |this: &mut Self, _: &mut Window, cx: &mut Context<Self>| {
                    this.toggle_always_select(cx)
                };
                Self::later(window, always_panel.clone(), cx, toggle);
            },
        );
        let compact = MenuEntry::new(
            checked_label("Compact Middle Packages", compact_middle_packages(cx)),
            move |window, cx| {
                let toggle = |this: &mut Self, _: &mut Window, cx: &mut Context<Self>| {
                    this.toggle_compact(cx)
                };
                Self::later(window, panel.clone(), cx, toggle);
            },
        );
        let appearance = MenuEntry::submenu("Tree Appearance", vec![compact]);
        let hiding = self.workspace.clone();
        let hide = MenuEntry::new("Hide", move |window, cx| {
            hiding
                .update(cx, |workspace, cx| {
                    workspace.toggle_panel(DockSide::Left, window, cx)
                })
                .ok();
        });
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.show_menu(
                    position,
                    vec![always_select, appearance, MenuEntry::separator(), hide],
                    cx,
                )
            })
            .ok();
    }

    /// Compact Middle Packages: the switch is a setting, and the settings file keeps it.
    fn toggle_compact(&mut self, cx: &mut Context<Self>) {
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.update_settings(
                    |settings| {
                        settings.project_compact_middle_packages =
                            !settings.project_compact_middle_packages;
                    },
                    cx,
                )
            })
            .ok();
        self.apply_compact(cx);
        cx.notify();
    }

    /// Brings the rows in line with the Compact Middle Packages setting. A selected folder
    /// that is now one of the folders of a chain selects the row that holds it. Returns
    /// whether the rows changed.
    fn apply_compact(&mut self, cx: &mut Context<Self>) -> bool {
        let on = compact_middle_packages(cx);
        if self.tree.compact() == on {
            return false;
        }
        self.tree.set_compact(on);
        let mut selection: Vec<PathBuf> = Vec::new();
        for path in &self.selection {
            let shown = match self.tree.index_of(path) {
                Some(index) => self.tree.rows()[index].path.clone(),
                None => path.clone(),
            };
            if !selection.contains(&shown) {
                selection.push(shown);
            }
        }
        self.selection = selection;
        self.anchor = self.selected_index();
        self.read_pending(cx);
        true
    }

    /// Takes the source roots from the project model of the run panel, which names
    /// the packages of the tree. Without a model the roots stay as they are. Returns whether
    /// they changed.
    fn sync_source_roots(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.runs_watched
            && let Some(runs) = cx
                .try_global::<crate::script::ActiveRunPanel>()
                .map(|active| active.0.clone())
        {
            cx.observe(&runs, |this, _, cx| {
                if this.sync_source_roots(cx) {
                    cx.notify();
                }
            })
            .detach();
            self.runs_watched = true;
        }
        let model = cx
            .try_global::<crate::script::ActiveRunPanel>()
            .map(|active| active.0.clone())
            .and_then(|runs| runs.read(cx).model());
        let Some(model) = model else {
            return false;
        };
        let roots: Vec<PathBuf> = model
            .modules
            .iter()
            .flat_map(|module| module.units.iter())
            .flat_map(|unit| unit.source_roots.iter().cloned())
            .collect();
        let changed = self.tree.set_source_roots(roots);
        if changed {
            self.read_pending(cx);
        }
        changed
    }

    /// Sets the source roots for a test, which has no project model to read them from.
    #[cfg(test)]
    pub(crate) fn set_source_roots_for_test(
        &mut self,
        roots: Vec<PathBuf>,
        cx: &mut Context<Self>,
    ) {
        if self.tree.set_source_roots(roots) {
            self.read_pending(cx);
        }
        cx.notify();
    }

    /// Always Select Opened File: the switch is a setting, and the settings file keeps it.
    /// Switching it on selects the file of the active tab at once.
    fn toggle_always_select(&mut self, cx: &mut Context<Self>) {
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.update_settings(
                    |settings| {
                        settings.project_always_select_opened_file =
                            !settings.project_always_select_opened_file;
                    },
                    cx,
                )
            })
            .ok();
        if self.always_select_opened_file(cx) {
            self.select_opened_file(cx);
        }
        cx.notify();
    }

    /// Whether the tree selects the file of every tab that becomes active.
    fn always_select_opened_file(&self, cx: &App) -> bool {
        cx.try_global::<Settings>()
            .is_some_and(|settings| settings.project_always_select_opened_file)
    }

    /// A tab became active and the tree follows the editor: its file is selected when the
    /// tree can show it. A file outside the project root is not in the tree.
    fn follow_opened_file(&mut self, path: &Path, cx: &mut Context<Self>) {
        if path.starts_with(self.tree.root()) {
            self.select_path(path, cx);
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

    /// The kinds of source file the New Class dialog lists: the main kind first, then the
    /// other kinds in the order the dialog shows them. The plain files, the folders and
    /// the package file are not kinds.
    fn source_kinds(&self) -> Vec<FileTemplate> {
        let mut kinds = Vec::new();
        if let Some(primary) = self.primary_source() {
            kinds.push(primary);
        }
        for label in SOURCE_KIND_ORDER {
            if let Some(template) = self
                .templates
                .iter()
                .find(|template| template.label == label)
            {
                kinds.push(template.clone());
            }
        }
        for template in &self.templates {
            if NEW_FIXED.contains(&template.label)
                || kinds.iter().any(|kind| kind.label == template.label)
            {
                continue;
            }
            kinds.push(template.clone());
        }
        kinds
    }

    /// The source kind the New list offers: the first one the application gave.
    fn primary_source(&self) -> Option<FileTemplate> {
        self.templates
            .iter()
            .find(|template| {
                !NEW_FIXED.contains(&template.label) && !SOURCE_KINDS.contains(&template.label)
            })
            .cloned()
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
        let kinds = if self
            .primary_source()
            .is_some_and(|primary| primary.label == template.label)
        {
            self.source_kinds()
        } else {
            Vec::new()
        };
        if kinds.len() > 1 {
            let prompts = kinds
                .iter()
                .map(|template| PromptKind {
                    label: template.label.into(),
                    icon: template.icon,
                })
                .collect::<Vec<_>>();
            let chosen = cx.try_global::<LastNewKind>().map_or(0, |last| last.0);
            let fallback = template.clone();
            let submit = move |name: String, kind: usize, cx: &mut App| {
                cx.set_global(LastNewKind(kind));
                let template = kinds.get(kind).cloned().unwrap_or_else(|| fallback.clone());
                let (template, folder) = (template.clone(), folder.clone());
                let (panel, handle) = (panel.clone(), handle);
                let name = name.clone();
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
                Prompt::show_with_kinds(
                    &title, "Name", prompts, chosen, submit, workspace, window, cx,
                )
            };
            self.workspace.update(cx, ask).ok();
            return;
        }
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
        if label == FLYWAY_LABEL {
            let folder = self
                .migrations
                .next
                .as_ref()
                .map(|next| next.folder.clone())
                .unwrap_or_else(|| self.root_path().join(folder));
            self.create_flyway_migration(&name, &folder, window, cx);
            return;
        }
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
        // A row is a drop target too: a small drag that ends on the row it started from is
        // not a copy.
        let onto_itself = target == dragged.path;
        let target_folder = if target_is_folder {
            target
        } else {
            target
                .parent()
                .map_or_else(|| self.root_path(), Path::to_path_buf)
        };
        // Ctrl held at the drop copies the item. The modifiers of the window are those of the
        // mouse-up that ends the drag.
        if window.modifiers().control {
            if onto_itself {
                return;
            }
            return self.copy_dropped(dragged.path, dragged.folder, target_folder, window, cx);
        }
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

    /// Ctrl+drop: the item is copied into the folder. A language file goes through the Copy
    /// Class window, as F5 does. Any other item keeps its name, or takes `name (1)` when that
    /// name is taken there. The disk is read and written in the background.
    fn copy_dropped(
        &mut self,
        from: PathBuf,
        folder: bool,
        target_folder: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !folder
            && from.is_file()
            && let Some(stem) = self.class_stem(&from, cx)
        {
            return self.ask_copy_class(from, target_folder, stem, false, window, cx);
        }
        let task = cx.background_executor().spawn(async move {
            let to = plan_drop_copy(&from, &target_folder, folder)?;
            copy_path(&from, &to).map_err(|error| error.to_string())?;
            Ok::<PathBuf, String>(to)
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            this.update_in(cx, |this, _, cx| match result {
                Ok(to) => {
                    this.select_path(&to, cx);
                    this.reload(cx);
                }
                Err(error) => this.report("Cannot copy", error, cx),
            })
            .ok();
        })
        .detach();
    }

    /// The Copy Class window for a language file: the name and the folder come from the
    /// window, and `cut` says whether the file moves rather than copies.
    fn ask_copy_class(
        &mut self,
        source: PathBuf,
        target: PathBuf,
        stem: String,
        cut: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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
                &source,
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
        self.show_new_menu_at(point(px(160.), px(120.)), cx);
    }

    /// The New menu for the selection, shown at `position` of the window.
    fn show_new_menu_at(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let selected = self.selected_row_path();
        let folder = target_folder(
            &self.root_path(),
            selected
                .as_ref()
                .map(|(path, folder)| (path.as_path(), *folder)),
        );
        let entries = self.new_submenu(folder, cx);
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.show_menu(position, entries, cx)
            })
            .ok();
    }

    /// The one row of the context menu that holds everything the tree makes.
    fn new_entries(&self, folder: PathBuf, cx: &mut Context<Self>) -> Vec<MenuEntry> {
        let mut new = MenuEntry::submenu("New", self.new_submenu(folder, cx)).icon("general/add");
        if let Some(hint) = Self::key_hint("project_panel::NewItem") {
            new = new.shortcut(hint);
        }
        vec![new]
    }

    /// One row of a new file template. The name of the file follows `label`.
    fn template_entry(
        template: FileTemplate,
        label: String,
        folder: PathBuf,
        panel: WeakEntity<Self>,
    ) -> MenuEntry {
        let icon = template.icon;
        MenuEntry::new(label, move |window, cx| {
            let (template, folder) = (template.clone(), folder.clone());
            let ask = |this: &mut Self, window: &mut Window, cx: &mut Context<Self>| {
                this.ask_new(template, folder, window, cx)
            };
            Self::later(window, panel.clone(), cx, ask);
        })
        .icon(icon)
    }

    /// The rows of the New list: the source kinds first, then the plain files and
    /// folders, then the rest of what the application offers.
    fn new_submenu(&self, folder: PathBuf, cx: &mut Context<Self>) -> Vec<MenuEntry> {
        let panel = cx.weak_entity();
        let find = |label: &str| {
            self.templates
                .iter()
                .find(|template| template.label == label)
                .cloned()
        };
        let mut entries = Vec::new();
        let primary = self.primary_source();
        let primary_label = primary.as_ref().map(|template| template.label.to_owned());
        if let Some(template) = primary {
            let label = format!("New {}", template.label);
            entries.push(Self::template_entry(
                template,
                label,
                folder.clone(),
                panel.clone(),
            ));
        }
        if let Some(template) = find("File") {
            entries.push(Self::template_entry(
                template,
                "New File".to_owned(),
                folder.clone(),
                panel.clone(),
            ));
        }
        {
            let scratch_panel = panel.clone();
            entries.push(
                MenuEntry::new("Scratch File", move |window, cx| {
                    let open = |this: &mut Self, window: &mut Window, cx: &mut Context<Self>| {
                        this.workspace
                            .update(cx, |workspace, cx| {
                                crate::window_popups::show_scratch_languages(workspace, window, cx)
                            })
                            .ok();
                    };
                    Self::later(window, scratch_panel.clone(), cx, open);
                })
                .icon("general/newFile"),
            );
        }
        if let Some(template) = find("Directory") {
            entries.push(Self::template_entry(
                template,
                "New Directory".to_owned(),
                folder.clone(),
                panel.clone(),
            ));
        }
        if let Some(template) = find("Package") {
            entries.push(Self::template_entry(
                template,
                "New Package".to_owned(),
                folder.clone(),
                panel.clone(),
            ));
        }
        if let Some(template) = find("package-info") {
            let label = format!("New {}.{}", template.label, template.extension);
            entries.push(Self::template_entry(
                template,
                label,
                folder.clone(),
                panel.clone(),
            ));
        }
        let mut rest: Vec<FileTemplate> = self
            .templates
            .iter()
            .filter(|template| {
                primary_label.as_deref() != Some(template.label)
                    && !NEW_FIXED.contains(&template.label)
                    && !SOURCE_KINDS.contains(&template.label)
            })
            .cloned()
            .collect();
        entries.push(MenuEntry::separator());
        for template in rest.drain(..) {
            let label = format!("New {}", template.label);
            entries.push(Self::template_entry(
                template,
                label,
                folder.clone(),
                panel.clone(),
            ));
        }
        if self.migrations.next.is_some() {
            let flyway_panel = panel.clone();
            entries.push(
                MenuEntry::new(format!("New {FLYWAY_LABEL}"), move |window, cx| {
                    let ask = |this: &mut Self, window: &mut Window, cx: &mut Context<Self>| {
                        this.ask_flyway_migration(window, cx)
                    };
                    Self::later(window, flyway_panel.clone(), cx, ask);
                })
                .icon("fileTypes/text"),
            );
        }
        let request_folder = folder.clone();
        let request_panel = panel.clone();
        entries.push(
            MenuEntry::new("New HTTP Request", move |window, cx| {
                let (template, folder) = (crate::tree_ops::http_request(), request_folder.clone());
                let ask = |this: &mut Self, window: &mut Window, cx: &mut Context<Self>| {
                    this.ask_new(template, folder, window, cx)
                };
                Self::later(window, request_panel.clone(), cx, ask);
            })
            .icon("fileTypes/text"),
        );
        entries
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

    /// The key of a row of the tree, as its menu writes it.
    fn key_hint(action: &str) -> Option<String> {
        let platform = keymap::platform();
        PANEL_KEYS
            .iter()
            .find(|row| row.action == action)
            .and_then(|row| row.keys_for(platform).first().copied())
            .map(|keys| keymap::shortcut_label(keys, platform))
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
            let mut open = MenuEntry::new("Open", move |window, cx| {
                let path = path.clone();
                let open = |this: &mut Self, window: &mut Window, cx: &mut Context<Self>| {
                    this.open_file(path, window, cx)
                };
                Self::later(window, panel.clone(), cx, open);
            });
            if let Some(hint) = Self::key_hint("project_panel::OpenRow") {
                open = open.shortcut(hint);
            }
            entries.push(open);
        }
        if is_folder {
            let (compare_panel, compare_left) = (panel.clone(), path.clone());
            let mut compare = MenuEntry::new("Compare With...", move |window, cx| {
                let left = compare_left.clone();
                let ask = |this: &mut Self, window: &mut Window, cx: &mut Context<Self>| {
                    this.compare_with_folder(left, window, cx)
                };
                Self::later(window, compare_panel.clone(), cx, ask);
            });
            if let Some(hint) = Self::key_hint("project_panel::CompareTwoFiles") {
                compare = compare.shortcut(hint);
            }
            entries.push(compare);
            let (diagram_panel, diagram_folder) = (panel.clone(), path.clone());
            entries.push(MenuEntry::new("Show Diagram", move |window, cx| {
                let folder = diagram_folder.clone();
                let show = |this: &mut Self, window: &mut Window, cx: &mut Context<Self>| {
                    this.workspace
                        .update(cx, |workspace, cx| {
                            crate::diagram::show(
                                workspace,
                                crate::diagram::DiagramSource::Folder(folder),
                                window,
                                cx,
                            )
                        })
                        .ok();
                };
                Self::later(window, diagram_panel.clone(), cx, show);
            }));
        }
        if !is_root {
            let (rename_panel, rename_path) = (panel.clone(), path.clone());
            let mut rename = MenuEntry::new("Rename...", move |window, cx| {
                let path = rename_path.clone();
                let ask = |this: &mut Self, window: &mut Window, cx: &mut Context<Self>| {
                    this.ask_rename(path, window, cx)
                };
                Self::later(window, rename_panel.clone(), cx, ask);
            });
            if let Some(hint) = Self::key_hint("project_panel::RenameRow") {
                rename = rename.shortcut(hint);
            }
            entries.push(rename);
            let delete_panel = panel;
            let mut delete = MenuEntry::new("Delete...", move |window, cx| {
                let ask = |this: &mut Self, window: &mut Window, cx: &mut Context<Self>| {
                    this.ask_delete_selection(window, cx)
                };
                Self::later(window, delete_panel.clone(), cx, ask);
            });
            if let Some(hint) = Self::key_hint("project_panel::DeleteRow") {
                delete = delete.shortcut(hint);
            }
            entries.push(delete);
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
        if is_folder {
            let folder = path.clone();
            let terminal_workspace = self.workspace.clone();
            entries.push(MenuEntry::new("Open in Terminal", move |window, cx| {
                let directory = folder.clone();
                let terminal = cx
                    .try_global::<crate::terminal_panel::ActiveTerminal>()
                    .map(|active| active.0.clone());
                if let Some(terminal) = terminal {
                    terminal.update(cx, |panel, cx| panel.open_tab_in(directory, window, cx));
                }
                terminal_workspace
                    .update(cx, |workspace, cx| {
                        workspace.show_tool(workspace::TERMINAL_TOOL, window, cx)
                    })
                    .ok();
            }));
        }
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
        } else {
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
            entries.push(MenuEntry::submenu("Git", vec![show]));
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

    /// The title of the window and its toolbar, as the Project window of IDEA shows them:
    /// New, Select Opened File, Expand All, Collapse All and the view options. Hide comes
    /// from the shared panel tools, which show it under the pointer like every tool window.
    fn header(&self, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let workspace = self.workspace.clone();
        let hide = move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
            workspace
                .update(cx, |workspace, cx| {
                    workspace.toggle_panel(DockSide::Left, window, cx)
                })
                .ok();
        };
        let actions = div()
            .flex()
            .items_center()
            .gap(px(SPACE_XS))
            .child(
                tool_button(
                    "project-new",
                    "general/add",
                    "New",
                    Self::key_hint("project_panel::NewItem").map(Into::into),
                    theme,
                )
                .on_click(cx.listener(|this, event: &ClickEvent, _, cx| {
                    this.show_new_menu_at(event.position(), cx)
                })),
            )
            .child(
                tool_button(
                    "project-select-opened",
                    "general/locate",
                    "Select Opened File",
                    window_hint("workspace::SelectInProject").map(Into::into),
                    theme,
                )
                .on_click(cx.listener(|this, _, _, cx| this.select_opened_file(cx))),
            )
            .child(
                tool_button(
                    "project-expand-all",
                    "general/expandAll",
                    "Expand All",
                    Self::key_hint("project_panel::ExpandAllRows").map(Into::into),
                    theme,
                )
                .on_click(cx.listener(|this, _, _, cx| this.expand_fully(cx))),
            )
            .child(
                tool_button(
                    "project-collapse-all",
                    "general/collapseAll",
                    "Collapse All",
                    Self::key_hint("project_panel::CollapseAllRows").map(Into::into),
                    theme,
                )
                .on_click(cx.listener(|this, _, _, cx| this.collapse_all(cx))),
            );
        let options = cx
            .listener(|this, event: &ClickEvent, _, cx| this.show_view_menu(event.position(), cx));
        tool_header(
            div().font_weight(FontWeight::MEDIUM).child("Project"),
            actions,
            "left-panel",
            "project",
            options,
            hide,
            theme,
        )
        .debug_selector(|| "project-header".to_owned())
    }

    fn rows(&self, range: Range<usize>, cx: &mut Context<Self>) -> Vec<Div> {
        let theme = *cx.global::<Theme>();
        let coverage = cx
            .try_global::<crate::script::ActiveRunPanel>()
            .and_then(|panel| panel.0.read(cx).coverage());
        range
            .map(|index| {
                let row = &self.tree.rows()[index];
                let selected = self.selection.contains(&row.path);
                let name = row.name.clone();
                let hint = if index == 0 {
                    Some(row.path.display().to_string())
                } else {
                    coverage_text(coverage.as_deref(), &row.path, row.folder)
                };
                let style = TreeRow {
                    id: ("tree", index).into(),
                    depth: row.depth,
                    label: row.name.clone().into(),
                    hint: hint.map(Into::into),
                    icon: self.row_icon(index),
                    expandable: row.folder,
                    open: row.open,
                    selected,
                };
                let tint = cx
                    .try_global::<crate::git::ActiveGit>()
                    .and_then(|git| git.0.read(cx).kind_of(&row.path));
                let issue = self.migrations.issue_of(&row.path).is_some();
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
                    .when_some(tint, |this, kind| this.text_color(kind.color(&theme)))
                    .when(issue, |this| {
                        this.text_color(theme.stripe_error).underline()
                    })
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

/// The percentage of the covered lines of a file, or of every file under a folder, as the
/// project tree shows it after a run with coverage.
fn coverage_text(
    report: Option<&lang_api::CoverageReport>,
    path: &Path,
    folder: bool,
) -> Option<String> {
    let report = report?;
    let (covered, total) = if folder {
        report
            .files
            .iter()
            .filter(|file| file.path.starts_with(path))
            .fold((0, 0), |(covered, total), file| {
                let counters = file.line_counters();
                (covered + counters.covered, total + counters.total)
            })
    } else {
        let counters = report.file(path)?.line_counters();
        (counters.covered, counters.total)
    };
    let counters = lang_api::Counters { covered, total };
    counters.percent().map(|percent| format!("{percent}%"))
}

impl Render for ProjectPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        #[cfg(test)]
        workspace::note_view_render(cx, "project");
        // The rows follow the settings and the project model before they are drawn.
        self.apply_compact(cx);
        self.sync_source_roots(cx);
        let theme = *cx.global::<Theme>();
        let count = self.tree.rows().len();
        island(&theme)
            .id("project-panel")
            .key_context("ProjectPanel ToolWindow")
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
                this.compare_selection(window, cx)
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
            .on_action(cx.listener(|this, _: &ExpandAllRows, _, cx| this.expand_fully(cx)))
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
            .on_action(cx.listener(|this, _: &ReformatRows, window, cx| {
                let paths = this.selected_paths();
                this.workspace
                    .update(cx, |workspace, cx| {
                        crate::reformat::show_paths_dialog(workspace, paths, window, cx)
                    })
                    .ok();
            }))
            .on_key_down(cx.listener(|this, event, _, cx| this.key(event, cx)))
            .child(self.header(&theme, cx))
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

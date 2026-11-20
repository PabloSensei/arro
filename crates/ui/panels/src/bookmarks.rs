//! The Bookmarks tool window: the groups of the project, the files under them and the
//! bookmarks of each file. A bookmark opens on Enter or a double click, Delete takes it
//! away and F2 names it.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use editor_view::{BookmarkGroup, BookmarkPlace, BookmarkRevision, Spot};
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use workspace::{BOOKMARKS_TOOL, BookmarkGroupState, BookmarkLayout, BookmarkState, Workspace};

use crate::navigation;
use crate::outline::OpenEditors;
use crate::prompt::Prompt;

actions!(
    bookmarks_panel,
    [OpenBookmark, DeleteBookmark, RenameBookmark]
);

const CONTEXT: &str = "BookmarksPanel";
const GROUP: &str = "bookmarks-panel";
const ROW_MARGIN: f32 = SPACE_L;
/// A pause this long starts a new speed search rather than adding to the old one.
const SEARCH_TIMEOUT: Duration = Duration::from_millis(1000);
const FOLDER_ICON: &str = "nodes/folder";
const BOOKMARK_ICON: &str = "toolwindows/bookmarks";
/// Characters of a line the tree shows.
const SNIPPET: usize = 120;

macro_rules! bookmark_keys {
    ($(($keys:literal, $action:ident, $idea:expr, $mac:expr)),* $(,)?) => {
        pub fn bind_keys(cx: &mut App) {
            bind_keys_for(cx, keymap::platform());
        }

        fn bind_keys_for(cx: &mut App, platform: keymap::Platform) {
            $(
                let row = workspace::KeyRow {
                    keys: $keys,
                    mac: $mac,
                    action: concat!("bookmarks_panel::", stringify!($action)),
                    context: Some(CONTEXT),
                    idea: $idea,
                };
                for key in row.keys_for(platform) {
                    cx.bind_keys([KeyBinding::new(key, $action, Some(CONTEXT))]);
                }
            )*
        }

        #[cfg(test)]
        pub(crate) const KEYS: &[workspace::KeyRow] = &[
            $(workspace::KeyRow {
                keys: $keys,
                mac: $mac,
                action: concat!("bookmarks_panel::", stringify!($action)),
                context: Some(CONTEXT),
                idea: $idea,
            }),*
        ];
    };
}

bookmark_keys![
    ("f2", RenameBookmark, None, Some("f2")),
    ("delete", DeleteBookmark, None, Some("delete")),
    ("enter", OpenBookmark, None, Some("enter")),
];

/// One row of the tree, in the order it is drawn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Row {
    Group {
        group: usize,
    },
    File {
        group: usize,
        path: PathBuf,
    },
    Bookmark {
        group: usize,
        path: PathBuf,
        line: u32,
    },
}

pub struct BookmarksPanel {
    workspace: WeakEntity<Workspace>,
    focus: FocusHandle,
    scroll: ScrollHandle,
    rows: Vec<Row>,
    selected: Option<usize>,
    /// The groups the user folded away.
    closed_groups: HashSet<usize>,
    /// The files the user folded away.
    closed_files: HashSet<PathBuf>,
    /// The letters typed for the speed search.
    search: String,
    search_at: Option<Instant>,
    /// The text of the files no editor has open, read once for a change of the list.
    texts: HashMap<PathBuf, Vec<String>>,
    _revision: Subscription,
}

/// The Bookmarks window of the application. Only the tests look it up: the window itself
/// reaches the panel through its dock.
pub struct ActiveBookmarks(#[allow(dead_code)] pub Entity<BookmarksPanel>);

impl Global for ActiveBookmarks {}

impl BookmarksPanel {
    pub fn create(workspace: WeakEntity<Workspace>, cx: &mut App) -> Entity<Self> {
        cx.new(|cx| {
            let revision = cx.observe_global::<BookmarkRevision>(|this: &mut Self, cx| {
                this.texts.clear();
                this.refresh(cx);
                this.persist(cx);
                cx.notify();
            });
            let mut panel = Self {
                workspace,
                focus: cx.focus_handle(),
                scroll: ScrollHandle::new(),
                rows: Vec::new(),
                selected: None,
                closed_groups: HashSet::new(),
                closed_files: HashSet::new(),
                search: String::new(),
                search_at: None,
                texts: HashMap::new(),
                _revision: revision,
            };
            panel.refresh(cx);
            panel
        })
    }

    pub fn focus(&self) -> FocusHandle {
        self.focus.clone()
    }

    /// The rows of the tree, for the tests.
    #[cfg(test)]
    pub(crate) fn rows(&self) -> &[Row] {
        &self.rows
    }

    /// The row the keyboard is on, for the tests.
    #[cfg(test)]
    pub(crate) fn selected(&self) -> Option<usize> {
        self.selected
    }

    /// The text a row shows, for the tests.
    #[cfg(test)]
    pub(crate) fn row_label(&mut self, index: usize, cx: &App) -> Option<String> {
        let row = self.rows.get(index).cloned()?;
        Some(match row {
            Row::Group { group } => editor_view::bookmark_groups(cx)
                .get(group)
                .map(|group| group.name.clone())
                .unwrap_or_default(),
            Row::File { path, .. } => path
                .file_name()
                .map_or(String::new(), |name| name.to_string_lossy().into_owned()),
            Row::Bookmark { path, line, .. } => self.bookmark_label(&path, line, cx),
        })
    }

    /// The line of a bookmark as the tree writes it: its number from one and the text of
    /// the line. The mnemonic stands on the icon of the row.
    fn bookmark_label(&mut self, path: &Path, line: u32, cx: &App) -> String {
        let text = snippet(&self.line_text(path, line, cx));
        format!("{}: {text}", line + 1)
    }

    /// The groups and descriptions go to the saved layout of the project.
    fn persist(&mut self, cx: &mut Context<Self>) {
        let groups = editor_view::bookmark_groups(cx).to_vec();
        let default = editor_view::bookmark_default_group(cx);
        let layout = layout_of_groups(&groups, default);
        self.workspace
            .update(cx, |workspace, cx| workspace.set_bookmarks(layout, cx))
            .ok();
    }

    fn refresh(&mut self, cx: &App) {
        self.rows = self.build_rows(cx);
        if self.selected.is_some_and(|index| index >= self.rows.len()) {
            self.selected = None;
        }
    }

    fn build_rows(&self, cx: &App) -> Vec<Row> {
        let groups = editor_view::bookmark_groups(cx);
        let mut rows = Vec::new();
        for (group_index, group) in groups.iter().enumerate() {
            rows.push(Row::Group { group: group_index });
            if self.closed_groups.contains(&group_index) {
                continue;
            }
            let mut files: BTreeMap<&Path, Vec<&BookmarkPlace>> = BTreeMap::new();
            for place in &group.bookmarks {
                files.entry(place.path.as_path()).or_default().push(place);
            }
            for (path, mut places) in files {
                places.sort_by_key(|place| place.line);
                rows.push(Row::File {
                    group: group_index,
                    path: path.to_path_buf(),
                });
                if self.closed_files.contains(path) {
                    continue;
                }
                for place in places {
                    rows.push(Row::Bookmark {
                        group: group_index,
                        path: place.path.clone(),
                        line: place.line,
                    });
                }
            }
        }
        rows
    }

    fn place(&self, group: usize, path: &Path, line: u32, cx: &App) -> Option<BookmarkPlace> {
        editor_view::bookmark_groups(cx)
            .get(group)?
            .bookmarks
            .iter()
            .find(|place| place.path == path && place.line == line)
            .cloned()
    }

    /// The text of a line: the unsaved text of an open editor, the file otherwise.
    fn line_text(&mut self, path: &Path, line: u32, cx: &App) -> String {
        if let Some(editor) = OpenEditors::get(cx, path) {
            return editor.read(cx).line_text(line as usize).unwrap_or_default();
        }
        if !self.texts.contains_key(path) {
            let text = std::fs::read_to_string(path).unwrap_or_default();
            let lines = text.lines().map(str::to_owned).collect();
            self.texts.insert(path.to_path_buf(), lines);
        }
        self.texts
            .get(path)
            .and_then(|lines| lines.get(line as usize))
            .cloned()
            .unwrap_or_default()
    }

    fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.rows.is_empty() {
            return;
        }
        let last = self.rows.len() as isize - 1;
        let next = match self.selected {
            Some(index) => (index as isize + delta).clamp(0, last),
            None if delta < 0 => last,
            None => 0,
        };
        self.selected = Some(next as usize);
        cx.notify();
    }

    fn activate_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.selected else {
            return;
        };
        self.activate(index, window, cx);
    }

    fn activate(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(row) = self.rows.get(index).cloned() else {
            return;
        };
        self.selected = Some(index);
        match row {
            Row::Bookmark { path, line, .. } => self.go(path, line, window, cx),
            Row::Group { group } => {
                if !self.closed_groups.remove(&group) {
                    self.closed_groups.insert(group);
                }
                self.refresh(cx);
                cx.notify();
            }
            Row::File { path, .. } => {
                if !self.closed_files.remove(&path) {
                    self.closed_files.insert(path);
                }
                self.refresh(cx);
                cx.notify();
            }
        }
    }

    /// Enter on a group or a file opens it, as the arrow of the tree does.
    fn set_open(&mut self, index: usize, open: bool, cx: &mut Context<Self>) {
        let Some(row) = self.rows.get(index).cloned() else {
            return;
        };
        match row {
            Row::Group { group } => {
                if open {
                    self.closed_groups.remove(&group);
                } else {
                    self.closed_groups.insert(group);
                }
            }
            Row::File { path, .. } => {
                if open {
                    self.closed_files.remove(&path);
                } else {
                    self.closed_files.insert(path);
                }
            }
            Row::Bookmark { .. } => return,
        }
        self.refresh(cx);
        cx.notify();
    }

    fn go(&mut self, path: PathBuf, line: u32, window: &mut Window, cx: &mut Context<Self>) {
        let workspace = self.workspace.clone();
        let handle = window.window_handle();
        // The window is being updated while the panel handles the click, so the move waits
        // until it is free again.
        cx.defer(move |cx| {
            handle
                .update(cx, |_, window, cx| {
                    workspace
                        .update(cx, |workspace, cx| {
                            let spot = Spot::Line {
                                line: line as usize,
                                column: 0,
                            };
                            navigation::go_to(workspace, path, Some(spot), true, window, cx)
                        })
                        .ok();
                })
                .ok();
        });
    }

    fn delete_selected(&mut self, cx: &mut Context<Self>) {
        let Some(index) = self.selected else {
            return;
        };
        let Some(Row::Bookmark { path, line, .. }) = self.rows.get(index).cloned() else {
            return;
        };
        self.delete_bookmark(&path, line, cx);
    }

    fn delete_bookmark(&mut self, path: &Path, line: u32, cx: &mut Context<Self>) {
        if let Some(editor) = OpenEditors::get(cx, path) {
            editor.update(cx, |editor, cx| {
                editor.remove_bookmark_at_line(line as usize, cx)
            });
        } else {
            editor_view::remove_bookmark(path, line, cx);
        }
        self.refresh(cx);
        cx.notify();
    }

    fn move_to_group(&mut self, path: &Path, line: u32, group: usize, cx: &mut Context<Self>) {
        editor_view::move_bookmark_to_group(path, line, group, cx);
        self.refresh(cx);
        cx.notify();
    }

    fn rename_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.selected else {
            return;
        };
        match self.rows.get(index).cloned() {
            Some(Row::Group { group }) => self.rename_group(group, window, cx),
            Some(Row::Bookmark { path, line, .. }) => self.rename_bookmark(path, line, window, cx),
            _ => {}
        }
    }

    fn rename_bookmark(
        &mut self,
        path: PathBuf,
        line: u32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let current = editor_view::bookmark_groups(cx)
            .iter()
            .flat_map(|group| &group.bookmarks)
            .find(|place| place.path == path && place.line == line)
            .and_then(|place| place.description.clone())
            .unwrap_or_default();
        let workspace = self.workspace.clone();
        workspace
            .update(cx, |workspace, cx| {
                let path = path.clone();
                Prompt::show_with(
                    "Rename Bookmark",
                    &current,
                    move |text, cx| editor_view::set_bookmark_description(&path, line, &text, cx),
                    workspace,
                    window,
                    cx,
                );
            })
            .ok();
    }

    fn rename_group(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(name) = editor_view::bookmark_groups(cx)
            .get(index)
            .map(|group| group.name.clone())
        else {
            return;
        };
        let workspace = self.workspace.clone();
        workspace
            .update(cx, |workspace, cx| {
                Prompt::show_with(
                    "Rename Group",
                    &name,
                    move |text, cx| editor_view::rename_bookmark_group(index, &text, cx),
                    workspace,
                    window,
                    cx,
                );
            })
            .ok();
    }

    fn new_group(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let workspace = self.workspace.clone();
        workspace
            .update(cx, |workspace, cx| {
                Prompt::show(
                    "New Group",
                    "",
                    |name, cx| editor_view::add_bookmark_group(&name, cx),
                    workspace,
                    window,
                    cx,
                );
            })
            .ok();
    }

    fn make_default_group(&mut self, index: usize, cx: &mut Context<Self>) {
        editor_view::make_bookmark_group_default(index, cx);
        self.refresh(cx);
        cx.notify();
    }

    fn delete_group(&mut self, index: usize, cx: &mut Context<Self>) {
        editor_view::delete_bookmark_group(index, cx);
        self.refresh(cx);
        cx.notify();
    }

    fn menu(
        &mut self,
        index: usize,
        position: Point<Pixels>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(row) = self.rows.get(index).cloned() else {
            return;
        };
        self.selected = Some(index);
        let panel = cx.weak_entity();
        let mut entries = Vec::new();
        match row {
            Row::Bookmark { group, path, line } => {
                let (jump_path, jump_line) = (path.clone(), line);
                let jump_panel = panel.clone();
                entries.push(
                    MenuEntry::new("Jump to Source", move |window, cx| {
                        jump_panel
                            .update(cx, |this, cx| {
                                this.go(jump_path.clone(), jump_line, window, cx)
                            })
                            .ok();
                    })
                    .icon("general/openInToolWindow"),
                );
                let rename_panel = panel.clone();
                let (rename_path, rename_line) = (path.clone(), line);
                entries.push(MenuEntry::new("Rename", move |window, cx| {
                    rename_panel
                        .update(cx, |this, cx| {
                            this.rename_bookmark(rename_path.clone(), rename_line, window, cx)
                        })
                        .ok();
                }));
                let delete_panel = panel.clone();
                let (delete_path, delete_line) = (path.clone(), line);
                entries.push(
                    MenuEntry::new("Delete", move |_, cx| {
                        delete_panel
                            .update(cx, |this, cx| {
                                this.delete_bookmark(&delete_path, delete_line, cx)
                            })
                            .ok();
                    })
                    .icon("general/delete"),
                );

                let default = editor_view::bookmark_default_group(cx);
                let moves: Vec<_> = editor_view::bookmark_groups(cx)
                    .iter()
                    .enumerate()
                    .filter(|(target, _)| *target != group)
                    .map(|(target, group)| {
                        let panel = panel.clone();
                        let (path, line) = (path.clone(), line);
                        MenuEntry::new(group.name.clone(), move |_, cx| {
                            panel
                                .update(cx, |this, cx| this.move_to_group(&path, line, target, cx))
                                .ok();
                        })
                        .icon(if target == default {
                            "general/check"
                        } else {
                            FOLDER_ICON
                        })
                    })
                    .collect();
                if !moves.is_empty() {
                    entries.push(MenuEntry::submenu("Move to Group", moves).icon(FOLDER_ICON));
                }

                let new_panel = panel.clone();
                entries.push(
                    MenuEntry::new("New Group", move |window, cx| {
                        new_panel
                            .update(cx, |this, cx| this.new_group(window, cx))
                            .ok();
                    })
                    .icon("general/add"),
                );
            }
            Row::Group { group } => {
                let new_panel = panel.clone();
                entries.push(
                    MenuEntry::new("New Group", move |window, cx| {
                        new_panel
                            .update(cx, |this, cx| this.new_group(window, cx))
                            .ok();
                    })
                    .icon("general/add"),
                );
                let rename_panel = panel.clone();
                entries.push(MenuEntry::new("Rename", move |window, cx| {
                    rename_panel
                        .update(cx, |this, cx| this.rename_group(group, window, cx))
                        .ok();
                }));
                let default = editor_view::bookmark_default_group(cx);
                if group == default {
                    entries.push(MenuEntry::disabled("Make Default Group"));
                } else {
                    let default_panel = panel.clone();
                    entries.push(MenuEntry::new("Make Default Group", move |_, cx| {
                        default_panel
                            .update(cx, |this, cx| this.make_default_group(group, cx))
                            .ok();
                    }));
                }
                if editor_view::bookmark_groups(cx).len() < 2 {
                    entries.push(MenuEntry::disabled("Delete Group"));
                } else {
                    let delete_panel = panel.clone();
                    entries.push(
                        MenuEntry::new("Delete Group", move |_, cx| {
                            delete_panel
                                .update(cx, |this, cx| this.delete_group(group, cx))
                                .ok();
                        })
                        .icon("general/delete"),
                    );
                }
            }
            Row::File { .. } => return,
        }
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.show_menu(position, entries, cx)
            })
            .ok();
    }

    /// Typed letters pick the next row that starts with them.
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
        if self.search.is_empty() || self.rows.is_empty() {
            return true;
        }
        let needle = self.search.to_lowercase();
        let count = self.rows.len();
        let start = if expired {
            0
        } else {
            self.selected.map_or(0, |index| index + 1)
        };
        let found = (0..count).map(|step| (start + step) % count).find(|index| {
            self.search_text(*index, cx)
                .to_lowercase()
                .starts_with(&needle)
        });
        if let Some(index) = found {
            self.selected = Some(index);
            cx.notify();
        }
        true
    }

    fn search_text(&mut self, index: usize, cx: &App) -> String {
        let Some(row) = self.rows.get(index).cloned() else {
            return String::new();
        };
        match row {
            Row::Group { group } => editor_view::bookmark_groups(cx)
                .get(group)
                .map(|group| group.name.clone())
                .unwrap_or_default(),
            Row::File { path, .. } => path
                .file_name()
                .map_or(String::new(), |name| name.to_string_lossy().into_owned()),
            Row::Bookmark { group, path, line } => {
                let text = self.line_text(&path, line, cx);
                let place = self.place(group, &path, line, cx);
                let mut parts = Vec::new();
                if let Some(letter) = place.as_ref().and_then(|place| place.mnemonic) {
                    parts.push(letter.to_string());
                }
                parts.push(text);
                if let Some(description) = place.and_then(|place| place.description) {
                    parts.push(description);
                }
                parts.join(" ")
            }
        }
    }

    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let key = &event.keystroke;
        match key.key.as_str() {
            "up" => {
                cx.stop_propagation();
                self.move_selection(-1, cx);
            }
            "down" => {
                cx.stop_propagation();
                self.move_selection(1, cx);
            }
            "left" => {
                cx.stop_propagation();
                let index = self.selected.unwrap_or(0);
                self.set_open(index, false, cx);
            }
            "right" => {
                cx.stop_propagation();
                let index = self.selected.unwrap_or(0);
                self.set_open(index, true, cx);
            }
            "enter" => {
                cx.stop_propagation();
                self.activate_selected(window, cx);
            }
            "delete" => {
                cx.stop_propagation();
                self.delete_selected(cx);
            }
            "f2" => {
                cx.stop_propagation();
                self.rename_selected(window, cx);
            }
            "escape" => {
                if !self.search.is_empty() {
                    self.search.clear();
                    self.search_at = None;
                    cx.stop_propagation();
                }
            }
            _ => {
                if self.search_key(event, cx) {
                    cx.stop_propagation();
                }
            }
        }
    }
}

/// The bookmarks of the project groups, with the group a new bookmark goes to.
pub(crate) fn groups_from_layout(
    layout: &BookmarkLayout,
    project: &str,
) -> (Vec<BookmarkGroup>, usize) {
    let places = |list: &[BookmarkState]| {
        list.iter()
            .map(|bookmark| BookmarkPlace {
                path: bookmark.path.clone(),
                line: bookmark.line,
                mnemonic: bookmark.mnemonic,
                description: bookmark.description.clone(),
            })
            .collect::<Vec<_>>()
    };
    match layout {
        BookmarkLayout::Grouped { groups, default } if !groups.is_empty() => {
            let groups: Vec<_> = groups
                .iter()
                .map(|group| BookmarkGroup {
                    name: if group.name.trim().is_empty() {
                        project.to_owned()
                    } else {
                        group.name.clone()
                    },
                    bookmarks: places(&group.bookmarks),
                })
                .collect();
            let default = (*default).min(groups.len() - 1);
            (groups, default)
        }
        BookmarkLayout::Flat(list) => (
            vec![BookmarkGroup {
                name: project.to_owned(),
                bookmarks: places(list),
            }],
            0,
        ),
        BookmarkLayout::ByFile(files) => {
            let bookmarks = files
                .iter()
                .flat_map(|(path, lines)| {
                    lines.iter().map(move |(line, mnemonic)| BookmarkPlace {
                        path: path.clone(),
                        line: *line,
                        mnemonic: *mnemonic,
                        description: None,
                    })
                })
                .collect();
            (
                vec![BookmarkGroup {
                    name: project.to_owned(),
                    bookmarks,
                }],
                0,
            )
        }
        BookmarkLayout::Grouped { groups, .. } => {
            // A layout that saved no group still has the bookmarks of the files the user
            // visited before the groups existed.
            let bookmarks = groups
                .iter()
                .flat_map(|group| places(&group.bookmarks))
                .collect();
            (
                vec![BookmarkGroup {
                    name: project.to_owned(),
                    bookmarks,
                }],
                0,
            )
        }
    }
}

/// What the layout keeps of the groups of the window.
pub(crate) fn layout_of_groups(groups: &[BookmarkGroup], default: usize) -> BookmarkLayout {
    let groups = groups
        .iter()
        .map(|group| BookmarkGroupState {
            name: group.name.clone(),
            bookmarks: group
                .bookmarks
                .iter()
                .map(|place| BookmarkState {
                    path: place.path.clone(),
                    line: place.line,
                    mnemonic: place.mnemonic,
                    description: place.description.clone(),
                })
                .collect(),
        })
        .collect();
    BookmarkLayout::grouped(groups, default)
}

/// The command of the Bookmarks button of the right stripe: the window opens.
pub fn show_window(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    workspace.show_tool(BOOKMARKS_TOOL, window, cx);
}

fn snippet(text: &str) -> String {
    let text = text.trim();
    let mut shown: String = text.chars().take(SNIPPET).collect();
    if text.chars().count() > SNIPPET {
        shown.push('\u{2026}');
    }
    shown
}

impl Render for BookmarksPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let workspace = self.workspace.clone();
        let options =
            crate::tool_menu::options_menu(workspace.clone(), BOOKMARKS_TOOL, |_| Vec::new());
        let hide = move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
            workspace
                .update(cx, |workspace, cx| {
                    workspace.toggle_tool(BOOKMARKS_TOOL, window, cx)
                })
                .ok();
        };
        let default = editor_view::bookmark_default_group(cx);
        let mut rows = Vec::with_capacity(self.rows.len());
        for index in 0..self.rows.len() {
            let row = self.rows[index].clone();
            let (depth, icon, label, hint, expandable, open, mnemonic) = match &row {
                Row::Group { group: group_index } => {
                    let (name, is_default) = editor_view::bookmark_groups(cx)
                        .get(*group_index)
                        .map_or((String::new(), false), |group| {
                            (group.name.clone(), *group_index == default)
                        });
                    (
                        0,
                        FOLDER_ICON,
                        name,
                        is_default.then(|| "default".to_owned()),
                        true,
                        !self.closed_groups.contains(group_index),
                        None,
                    )
                }
                Row::File { path, .. } => {
                    let icon = self
                        .workspace
                        .upgrade()
                        .map_or("fileTypes/text", |workspace| {
                            workspace.read(cx).icons().icon_for(path)
                        });
                    (
                        1,
                        icon,
                        path.file_name()
                            .map_or(String::new(), |name| name.to_string_lossy().into_owned()),
                        None,
                        true,
                        !self.closed_files.contains(path),
                        None,
                    )
                }
                Row::Bookmark { group, path, line } => {
                    let place = self.place(*group, path, *line, cx);
                    let label = self.bookmark_label(path, *line, cx);
                    let hint = place.as_ref().and_then(|place| place.description.clone());
                    let mnemonic = place.and_then(|place| place.mnemonic);
                    (2, BOOKMARK_ICON, label, hint, false, false, mnemonic)
                }
            };
            let selected = self.selected == Some(index);
            let style = TreeRow {
                id: ("bookmark-row", index).into(),
                depth,
                label: label.into(),
                hint: hint.map(Into::into),
                icon,
                expandable,
                open,
                selected,
            };
            let selector = match &row {
                Row::Group { .. } => format!("bookmark-group-row-{index}"),
                Row::File { .. } => format!("bookmark-file-row-{index}"),
                Row::Bookmark { .. } => format!("bookmark-item-row-{index}"),
            };
            let tree = tree_row(style, &theme)
                .debug_selector(move || selector)
                .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                    window.focus(&this.focus, cx);
                    this.selected = Some(index);
                    if event.click_count() >= 2 {
                        this.activate(index, window, cx);
                    }
                    cx.notify();
                }))
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                        this.selected = Some(index);
                        this.menu(index, event.position, window, cx);
                        cx.notify();
                    }),
                );
            // The mnemonic of a bookmark stands on its icon, as the gutter of IDEA shows it.
            let badge = mnemonic.map(|letter| {
                div()
                    .id(("bookmark-mnemonic", index))
                    .debug_selector(move || format!("bookmark-mnemonic-{index}"))
                    .absolute()
                    .left(px(depth as f32 * 19. + ROW_PADDING_X + ICON + SPACE_S + 8.))
                    .top(px(11.))
                    .size(px(10.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(2.))
                    .bg(theme.popup_bg)
                    .text_size(px(8.))
                    .text_color(theme.accent)
                    .child(letter.to_uppercase().to_string())
            });
            let tree = div().relative().child(tree).children(badge);
            rows.push(div().w_full().px(px(ROW_MARGIN)).child(tree));
        }
        island(&theme)
            .id("bookmarks-panel")
            .debug_selector(|| "bookmarks-panel".to_owned())
            .key_context("BookmarksPanel ToolWindow")
            .track_focus(&self.focus)
            .group(GROUP)
            .h_full()
            .on_key_down(cx.listener(|this, event, window, cx| this.key(event, window, cx)))
            .on_action(
                cx.listener(|this, _: &OpenBookmark, window, cx| {
                    this.activate_selected(window, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &DeleteBookmark, _, cx| this.delete_selected(cx)))
            .on_action(
                cx.listener(|this, _: &RenameBookmark, window, cx| {
                    this.rename_selected(window, cx)
                }),
            )
            .child(tool_header(
                div().font_weight(FontWeight::MEDIUM).child("Bookmarks"),
                icon_button(
                    "bookmark-new-group",
                    "general/add",
                    "New Group",
                    None,
                    &theme,
                )
                .on_click(cx.listener(|this, _, window, cx| this.new_group(window, cx))),
                GROUP,
                "bookmarks",
                options,
                hide,
                &theme,
            ))
            .child(if self.rows.is_empty() {
                self.empty_state(&theme, cx)
            } else {
                scrollable(
                    "bookmark-tree",
                    &self.scroll,
                    div()
                        .id("bookmark-tree")
                        .flex()
                        .flex_col()
                        .pt(px(SPACE_S))
                        .overflow_y_scroll()
                        .track_scroll(&self.scroll)
                        .children(rows),
                    &theme,
                    window,
                    cx,
                )
                .flex_1()
                .min_h_0()
                .into_any_element()
            })
    }
}

impl BookmarksPanel {
    /// The empty list: the text and the action that toggles a bookmark at the caret, as F11
    /// does.
    fn empty_state(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        div()
            .flex()
            .flex_col()
            .flex_1()
            .items_center()
            .justify_center()
            .gap(px(SPACE_S))
            .text_color(theme.text_secondary)
            .child("No bookmarks")
            .child(
                div()
                    .id("bookmarks-empty-toggle")
                    .debug_selector(|| "bookmarks-empty-toggle".to_owned())
                    .cursor_pointer()
                    .text_color(theme.accent)
                    .child("Toggle Bookmark (F11)")
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_caret_bookmark(cx))),
            )
            .into_any_element()
    }

    /// Toggles the bookmark at the caret of the active editor.
    fn toggle_caret_bookmark(&mut self, cx: &mut Context<Self>) {
        self.workspace
            .update(cx, |workspace, cx| {
                let Some((_, view)) = workspace.active_editor() else {
                    return;
                };
                if let Ok(editor) = view.downcast::<editor_view::EditorView>() {
                    editor.update(cx, |view, cx| view.toggle_bookmark(None, cx));
                }
            })
            .ok();
    }
}

//! The bookmarks of the window: the groups, the line each bookmark points at and the
//! description the user wrote for it.

use std::path::{Path, PathBuf};

use ui_kit::*;

/// One bookmark: a line of a file and what the user wrote about it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BookmarkPlace {
    pub path: PathBuf,
    /// Buffer line from zero.
    pub line: u32,
    pub mnemonic: Option<char>,
    /// What the user wrote about the bookmark, shown beside its line.
    pub description: Option<String>,
}

/// A group of bookmarks, such as the one named after the project.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BookmarkGroup {
    pub name: String,
    pub bookmarks: Vec<BookmarkPlace>,
}

/// One bookmark as the lists of the window read it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BookmarkEntry {
    pub path: PathBuf,
    pub line: u32,
    pub mnemonic: Option<char>,
    pub description: Option<String>,
    /// The index of the group in [`bookmark_groups`].
    pub group: usize,
}

#[derive(Clone, Default)]
struct BookmarkStore {
    groups: Vec<BookmarkGroup>,
    default: usize,
}

impl Global for BookmarkStore {}

/// Bumped after every change of the bookmark list, so that the windows that show the
/// bookmarks follow the store without polling it.
#[derive(Clone, Copy, Debug, Default)]
pub struct BookmarkRevision(pub u64);

impl Global for BookmarkRevision {}

/// The groups of the window, in the order they were made.
pub fn bookmark_groups(cx: &App) -> &[BookmarkGroup] {
    cx.try_global::<BookmarkStore>()
        .map_or(&[], |store| store.groups.as_slice())
}

/// The index of the group a new bookmark goes to.
pub fn bookmark_default_group(cx: &App) -> usize {
    cx.try_global::<BookmarkStore>()
        .map_or(0, |store| store.default)
}

/// Every bookmark, files in path order.
pub fn bookmark_entries(cx: &App) -> Vec<BookmarkEntry> {
    let Some(store) = cx.try_global::<BookmarkStore>() else {
        return Vec::new();
    };
    let mut entries: Vec<_> = store
        .groups
        .iter()
        .enumerate()
        .flat_map(|(group, member)| {
            member.bookmarks.iter().map(move |place| BookmarkEntry {
                path: place.path.clone(),
                line: place.line,
                mnemonic: place.mnemonic,
                description: place.description.clone(),
                group,
            })
        })
        .collect();
    entries.sort_by(|a, b| (&a.path, a.line).cmp(&(&b.path, b.line)));
    entries
}

/// The lines of a file that carry a bookmark, in line order.
pub(crate) fn bookmark_lines(path: &Path, cx: &App) -> Vec<(u32, Option<char>)> {
    let Some(store) = cx.try_global::<BookmarkStore>() else {
        return Vec::new();
    };
    let mut lines: Vec<_> = store
        .groups
        .iter()
        .flat_map(|group| &group.bookmarks)
        .filter(|place| place.path == path)
        .map(|place| (place.line, place.mnemonic))
        .collect();
    lines.sort_by_key(|(line, _)| *line);
    lines.dedup_by_key(|(line, _)| *line);
    lines
}

/// Puts the groups of a saved project in place. An empty name takes the name of the
/// project, and a list with no groups gets one group named after it.
pub fn install_bookmarks(
    mut groups: Vec<BookmarkGroup>,
    default: usize,
    project: &str,
    cx: &mut App,
) {
    if groups.is_empty() {
        groups.push(BookmarkGroup {
            name: project.to_owned(),
            bookmarks: Vec::new(),
        });
    }
    for group in &mut groups {
        if group.name.trim().is_empty() {
            group.name = project.to_owned();
        }
        sort(&mut group.bookmarks);
    }
    let default = default.min(groups.len() - 1);
    cx.set_global(BookmarkStore { groups, default });
    bump(cx);
}

/// Makes a group. A name that is already taken is left alone.
pub fn add_bookmark_group(name: &str, cx: &mut App) {
    let name = name.trim();
    if name.is_empty() {
        return;
    }
    {
        let store = cx.default_global::<BookmarkStore>();
        if store.groups.iter().any(|group| group.name == name) {
            return;
        }
        store.groups.push(BookmarkGroup {
            name: name.to_owned(),
            bookmarks: Vec::new(),
        });
    }
    bump(cx);
}

/// Gives a group another name.
pub fn rename_bookmark_group(index: usize, name: &str, cx: &mut App) {
    let name = name.trim();
    if name.is_empty() {
        return;
    }
    {
        let store = cx.default_global::<BookmarkStore>();
        let Some(group) = store.groups.get_mut(index) else {
            return;
        };
        if group.name == name {
            return;
        }
        group.name = name.to_owned();
    }
    bump(cx);
}

/// Deletes a group and gives its bookmarks to the default one. The last group stays.
pub fn delete_bookmark_group(index: usize, cx: &mut App) {
    {
        let store = cx.default_global::<BookmarkStore>();
        if store.groups.len() < 2 || index >= store.groups.len() {
            return;
        }
        let target = if index == store.default {
            0
        } else if index < store.default {
            store.default - 1
        } else {
            store.default
        };
        let removed = store.groups.remove(index);
        let target = target.min(store.groups.len() - 1);
        store.default = target;
        store.groups[target].bookmarks.extend(removed.bookmarks);
        sort(&mut store.groups[target].bookmarks);
    }
    bump(cx);
}

/// Makes a group the one a new bookmark goes to.
pub fn make_bookmark_group_default(index: usize, cx: &mut App) {
    {
        let store = cx.default_global::<BookmarkStore>();
        if index >= store.groups.len() || store.default == index {
            return;
        }
        store.default = index;
    }
    bump(cx);
}

/// Moves a bookmark to another group.
pub fn move_bookmark_to_group(path: &Path, line: u32, group: usize, cx: &mut App) {
    {
        let store = cx.default_global::<BookmarkStore>();
        if group >= store.groups.len() {
            return;
        }
        let Some(place) = take_place(&mut store.groups, path, line) else {
            return;
        };
        store.groups[group].bookmarks.push(place);
        sort(&mut store.groups[group].bookmarks);
    }
    bump(cx);
}

/// Writes the description of a bookmark. An empty text takes the description away.
pub fn set_bookmark_description(path: &Path, line: u32, text: &str, cx: &mut App) {
    let text = text.trim();
    let description = (!text.is_empty()).then(|| text.to_owned());
    {
        let store = cx.default_global::<BookmarkStore>();
        let Some(place) = store
            .groups
            .iter_mut()
            .flat_map(|group| &mut group.bookmarks)
            .find(|place| place.path == path && place.line == line)
        else {
            return;
        };
        if place.description == description {
            return;
        }
        place.description = description;
    }
    bump(cx);
}

/// Takes a bookmark away without an editor: the next time its file opens, the line has
/// no mark.
pub fn remove_bookmark(path: &Path, line: u32, cx: &mut App) {
    {
        let store = cx.default_global::<BookmarkStore>();
        let mut taken = false;
        for group in &mut store.groups {
            let before = group.bookmarks.len();
            group
                .bookmarks
                .retain(|place| !(place.path == path && place.line == line));
            taken |= group.bookmarks.len() != before;
        }
        if !taken {
            return;
        }
    }
    bump(cx);
}

/// Takes the bookmark lines of a file from an editor that moved them with its text. The
/// descriptions and the groups stay on the bookmarks they belonged to.
pub(crate) fn replace_bookmark_lines(
    path: &Path,
    lines: &[(u32, Option<char>)],
    cx: &mut App,
) -> bool {
    let mut changed = false;
    {
        let store = cx.default_global::<BookmarkStore>();
        let rebuilt = rebuilt_groups(&store.groups, store.default, path, lines);
        if rebuilt != store.groups {
            store.groups = rebuilt;
            changed = true;
        }
    }
    if changed {
        bump(cx);
    }
    changed
}

/// The groups with the bookmarks of `path` replaced by `lines`. A line that did not move
/// keeps its description and its group; one that moved follows the nearest old line, and
/// a new one goes to the group the file was in.
fn rebuilt_groups(
    groups: &[BookmarkGroup],
    default: usize,
    path: &Path,
    lines: &[(u32, Option<char>)],
) -> Vec<BookmarkGroup> {
    struct Old {
        group: usize,
        place: BookmarkPlace,
    }
    let old: Vec<Old> = groups
        .iter()
        .enumerate()
        .flat_map(|(group, member)| {
            member
                .bookmarks
                .iter()
                .filter(|place| place.path == path)
                .cloned()
                .map(move |place| Old { group, place })
        })
        .collect();
    let file_group = old.first().map_or(default, |old| old.group);
    let mut used = vec![false; old.len()];
    let mut places = Vec::with_capacity(lines.len());
    for &(line, mnemonic) in lines {
        let pick = (0..old.len())
            .find(|&index| !used[index] && old[index].place.line == line)
            .or_else(|| {
                mnemonic.and_then(|letter| {
                    (0..old.len())
                        .find(|&index| !used[index] && old[index].place.mnemonic == Some(letter))
                })
            })
            .or_else(|| {
                (0..old.len())
                    .filter(|&index| !used[index])
                    .min_by_key(|&index| (old[index].place.line.abs_diff(line), index))
            });
        let (group, description) = match pick {
            Some(index) => {
                used[index] = true;
                (old[index].group, old[index].place.description.clone())
            }
            None => (file_group, None),
        };
        places.push((
            group,
            BookmarkPlace {
                path: path.to_path_buf(),
                line,
                mnemonic,
                description,
            },
        ));
    }
    let mut rebuilt = groups.to_vec();
    for group in &mut rebuilt {
        group.bookmarks.retain(|place| place.path != path);
    }
    // A window that never installed the groups, such as a bare editor, still keeps the
    // bookmarks of its files in one group.
    if rebuilt.is_empty() && !places.is_empty() {
        rebuilt.push(BookmarkGroup {
            name: "Bookmarks".to_owned(),
            bookmarks: Vec::new(),
        });
    }
    for (group, place) in places {
        let at = group.min(rebuilt.len().saturating_sub(1));
        rebuilt[at].bookmarks.push(place);
    }
    for group in &mut rebuilt {
        sort(&mut group.bookmarks);
    }
    rebuilt
}

/// Takes the bookmark of a line out of the group it is in.
fn take_place(groups: &mut [BookmarkGroup], path: &Path, line: u32) -> Option<BookmarkPlace> {
    for group in groups {
        if let Some(index) = group
            .bookmarks
            .iter()
            .position(|place| place.path == path && place.line == line)
        {
            return Some(group.bookmarks.remove(index));
        }
    }
    None
}

fn sort(places: &mut [BookmarkPlace]) {
    places.sort_by(|a, b| (&a.path, a.line).cmp(&(&b.path, b.line)));
}

fn bump(cx: &mut App) {
    let next = cx
        .try_global::<BookmarkRevision>()
        .map_or(1, |revision| revision.0.wrapping_add(1));
    cx.set_global(BookmarkRevision(next));
}

#[cfg(test)]
mod tests {
    use super::{BookmarkGroup, BookmarkPlace};
    use std::path::{Path, PathBuf};

    fn place(path: &str, line: u32, description: Option<&str>) -> BookmarkPlace {
        BookmarkPlace {
            path: PathBuf::from(path),
            line,
            mnemonic: None,
            description: description.map(str::to_owned),
        }
    }

    #[test]
    fn a_moved_line_keeps_its_description_and_its_group() {
        let path = Path::new("a");
        let groups = vec![
            BookmarkGroup {
                name: "project".to_owned(),
                bookmarks: Vec::new(),
            },
            BookmarkGroup {
                name: "later".to_owned(),
                bookmarks: vec![place("a", 2, Some("look"))],
            },
        ];
        let rebuilt = super::rebuilt_groups(&groups, 0, path, &[(5, None)]);
        assert_eq!(rebuilt[1].bookmarks, [place("a", 5, Some("look"))]);
        assert!(rebuilt[0].bookmarks.is_empty(), "the group stays with it");
    }

    #[test]
    fn a_new_bookmark_of_a_file_goes_to_the_group_of_the_file() {
        let path = Path::new("a");
        let groups = vec![
            BookmarkGroup {
                name: "project".to_owned(),
                bookmarks: Vec::new(),
            },
            BookmarkGroup {
                name: "later".to_owned(),
                bookmarks: vec![place("a", 2, None)],
            },
        ];
        let rebuilt = super::rebuilt_groups(&groups, 0, path, &[(2, None), (7, None)]);
        assert_eq!(rebuilt[1].bookmarks.len(), 2);
        assert!(rebuilt[0].bookmarks.is_empty());
    }
}

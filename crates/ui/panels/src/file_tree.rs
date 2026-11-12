//! The project tree as data: what was read from disk, what is expanded and the
//! flat list of visible rows. No UI and no I/O on the calling thread, except
//! [`read_folder`], which the panel runs in the background.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

/// Never shown: the object store of the version control.
const HIDDEN: &[&str] = &[".git"];

#[derive(Clone, Debug, PartialEq)]
pub struct Child {
    pub path: PathBuf,
    pub name: String,
    pub folder: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub path: PathBuf,
    pub name: String,
    pub depth: usize,
    pub folder: bool,
    pub open: bool,
}

/// Lists one folder: folders first, then files, by name ignoring case.
pub fn read_folder(folder: &Path) -> std::io::Result<Vec<Child>> {
    let mut children = Vec::new();
    for entry in std::fs::read_dir(folder)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if HIDDEN.contains(&name.as_str()) {
            continue;
        }
        let folder = entry.file_type().is_ok_and(|kind| kind.is_dir());
        children.push(Child {
            path: entry.path(),
            name,
            folder,
        });
    }
    children.sort_by_cached_key(|child| (!child.folder, child.name.to_lowercase()));
    Ok(children)
}

pub struct FileTree {
    root: PathBuf,
    listings: HashMap<PathBuf, Vec<Child>>,
    /// Rows that come from outside the folder on disk, such as Scratches.
    virtuals: HashMap<PathBuf, Vec<Child>>,
    expanded: BTreeSet<PathBuf>,
    rows: Vec<Row>,
}

impl FileTree {
    pub fn new(root: PathBuf, expanded: impl IntoIterator<Item = PathBuf>) -> Self {
        let mut expanded: BTreeSet<PathBuf> = expanded.into_iter().collect();
        expanded.insert(root.clone());
        let mut tree = Self {
            root,
            listings: HashMap::new(),
            virtuals: HashMap::new(),
            expanded,
            rows: Vec::new(),
        };
        tree.rebuild();
        tree
    }

    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn expanded(&self) -> Vec<PathBuf> {
        self.expanded.iter().cloned().collect()
    }

    /// Expanded folders on screen that have not been read yet.
    pub fn pending(&self) -> Vec<PathBuf> {
        let rows = self.rows.iter().filter(|row| row.open);
        let open = rows.map(|row| &row.path);
        open.filter(|path| !self.listings.contains_key(*path))
            .cloned()
            .collect()
    }

    pub fn has_listing(&self, folder: &Path) -> bool {
        self.listings.contains_key(folder)
    }

    /// Adds a row that is not read from `parent` on disk, such as the Scratches folder.
    pub fn add_virtual_child(&mut self, parent: &Path, child: Child) {
        self.virtuals
            .entry(parent.to_path_buf())
            .or_default()
            .push(child);
        self.rebuild();
    }

    pub fn set_listing(&mut self, folder: PathBuf, children: Vec<Child>) {
        self.listings.insert(folder, children);
        self.rebuild();
    }

    /// Replaces several listings and opens them in one rebuild, for Fully Expand.
    pub fn set_expanded_listings(&mut self, listings: Vec<(PathBuf, Vec<Child>)>) {
        for (folder, children) in listings {
            self.expanded.insert(folder.clone());
            self.listings.insert(folder, children);
        }
        self.rebuild();
    }

    pub fn toggle(&mut self, folder: &Path) {
        if !self.expanded.remove(folder) {
            self.expanded.insert(folder.to_path_buf());
        }
        self.rebuild();
    }

    pub fn set_expanded(&mut self, folder: &Path, open: bool) {
        if open != self.expanded.contains(folder) {
            self.toggle(folder);
        }
    }

    /// Closes every folder but the root, as Ctrl+NumPad Minus does.
    pub fn collapse_all(&mut self) {
        let root = self.root.clone();
        self.expanded.clear();
        self.expanded.insert(root);
        self.rebuild();
    }

    /// A file or folder got another name: folders opened inside it stay open under the
    /// new name, and what was read of it is read again.
    pub fn moved(&mut self, from: &Path, to: &Path) {
        let inside = |path: &PathBuf| path.starts_with(from);
        let renamed: Vec<PathBuf> = self
            .expanded
            .iter()
            .filter(|path| inside(path))
            .cloned()
            .collect();
        for path in renamed {
            self.expanded.remove(&path);
            if let Ok(rest) = path.strip_prefix(from) {
                self.expanded.insert(to.join(rest));
            }
        }
        self.listings.retain(|path, _| !inside(path));
        self.rebuild();
    }

    /// A file or folder is gone: nothing under it stays open or listed.
    pub fn forget(&mut self, path: &Path) {
        self.expanded.retain(|open| !open.starts_with(path));
        self.listings.retain(|folder, _| !folder.starts_with(path));
        if let Some(parent) = path.parent() {
            self.listings.remove(parent);
        }
        self.rebuild();
    }

    pub fn index_of(&self, path: &Path) -> Option<usize> {
        self.rows.iter().position(|row| row.path == path)
    }

    /// The row of the folder that holds `index`.
    pub fn parent_of(&self, index: usize) -> Option<usize> {
        let depth = self.rows.get(index)?.depth;
        (0..index).rev().find(|&row| self.rows[row].depth < depth)
    }

    fn rebuild(&mut self) {
        let name = self.root.file_name().unwrap_or_default().to_string_lossy();
        let mut rows = vec![Row {
            path: self.root.clone(),
            name: name.into_owned(),
            depth: 0,
            folder: true,
            open: self.expanded.contains(&self.root),
        }];
        if rows[0].open {
            self.append_children(&self.root, 1, &mut rows);
        }
        self.rows = rows;
    }

    fn append_children(&self, folder: &Path, depth: usize, rows: &mut Vec<Row>) {
        let mut children: Vec<_> = self
            .listings
            .get(folder)
            .into_iter()
            .flatten()
            .cloned()
            .collect();
        // Virtual rows stay after the real ones, so that a folder of the project keeps
        // the place the keys of the tree expect.
        children.extend(self.virtuals.get(folder).into_iter().flatten().cloned());
        for child in &children {
            let open = child.folder && self.expanded.contains(&child.path);
            rows.push(Row {
                path: child.path.clone(),
                name: child.name.clone(),
                depth,
                folder: child.folder,
                open,
            });
            if open {
                self.append_children(&child.path, depth + 1, rows);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn child(path: &str, folder: bool) -> Child {
        let path = PathBuf::from(path);
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        Child { path, name, folder }
    }

    fn names(tree: &FileTree) -> Vec<(usize, &str)> {
        let rows = tree.rows().iter();
        rows.map(|row| (row.depth, row.name.as_str())).collect()
    }

    fn sample() -> FileTree {
        let mut tree = FileTree::new(PathBuf::from("/p"), []);
        let listing = vec![child("/p/src", true), child("/p/pom.xml", false)];
        tree.set_listing(PathBuf::from("/p"), listing);
        tree
    }

    #[test]
    fn root_is_open_and_waits_for_its_listing() {
        let tree = FileTree::new(PathBuf::from("/p"), []);
        assert_eq!(names(&tree), [(0, "p")]);
        assert_eq!(tree.pending(), [PathBuf::from("/p")]);
    }

    #[test]
    fn expanding_asks_for_the_folder_once() {
        let mut tree = sample();
        assert!(tree.pending().is_empty());
        tree.toggle(Path::new("/p/src"));
        assert_eq!(tree.pending(), [PathBuf::from("/p/src")]);

        tree.set_listing(
            PathBuf::from("/p/src"),
            vec![child("/p/src/App.txt", false)],
        );
        assert!(tree.pending().is_empty());
        let expected = [(0, "p"), (1, "src"), (2, "App.txt"), (1, "pom.xml")];
        assert_eq!(names(&tree), expected);

        tree.toggle(Path::new("/p/src"));
        assert_eq!(names(&tree), [(0, "p"), (1, "src"), (1, "pom.xml")]);
        tree.toggle(Path::new("/p/src"));
        assert!(tree.pending().is_empty(), "the listing is kept");
    }

    #[test]
    fn restored_folders_are_read_level_by_level() {
        let expanded = [PathBuf::from("/p/src"), PathBuf::from("/p/src/main")];
        let mut tree = FileTree::new(PathBuf::from("/p"), expanded);
        assert_eq!(tree.pending(), [PathBuf::from("/p")]);
        tree.set_listing(PathBuf::from("/p"), vec![child("/p/src", true)]);
        assert_eq!(tree.pending(), [PathBuf::from("/p/src")]);
        tree.set_listing(PathBuf::from("/p/src"), vec![child("/p/src/main", true)]);
        assert_eq!(tree.pending(), [PathBuf::from("/p/src/main")]);
    }

    #[test]
    fn a_renamed_folder_keeps_its_open_children_and_is_read_again() {
        let mut tree = sample();
        tree.toggle(Path::new("/p/src"));
        tree.set_listing(PathBuf::from("/p/src"), vec![child("/p/src/main", true)]);
        tree.toggle(Path::new("/p/src/main"));

        tree.moved(Path::new("/p/src"), Path::new("/p/code"));
        assert_eq!(
            tree.expanded(),
            [
                PathBuf::from("/p"),
                PathBuf::from("/p/code"),
                PathBuf::from("/p/code/main")
            ]
        );
        assert!(
            tree.pending().is_empty(),
            "the old folder row is still listed above"
        );
        tree.set_listing(PathBuf::from("/p"), vec![child("/p/code", true)]);
        assert_eq!(tree.pending(), [PathBuf::from("/p/code")]);
    }

    #[test]
    fn a_deleted_folder_is_forgotten_with_what_was_open_in_it() {
        let mut tree = sample();
        tree.toggle(Path::new("/p/src"));
        tree.set_listing(PathBuf::from("/p/src"), vec![child("/p/src/main", true)]);
        tree.forget(Path::new("/p/src"));
        assert_eq!(tree.expanded(), [PathBuf::from("/p")]);
        assert_eq!(
            tree.pending(),
            [PathBuf::from("/p")],
            "the parent is read again"
        );
    }

    #[test]
    fn finds_the_parent_row() {
        let mut tree = sample();
        tree.toggle(Path::new("/p/src"));
        tree.set_listing(
            PathBuf::from("/p/src"),
            vec![child("/p/src/App.txt", false)],
        );
        assert_eq!(tree.parent_of(2), Some(1));
        assert_eq!(tree.parent_of(3), Some(0));
        assert_eq!(tree.parent_of(0), None);
    }

    #[test]
    fn folders_go_first_and_case_is_ignored() {
        let root = test_support::scratch("file-tree");
        std::fs::create_dir_all(root.join("zeta")).unwrap();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join("Beta.txt"), "").unwrap();
        std::fs::write(root.join("alpha.txt"), "").unwrap();
        let names: Vec<String> = read_folder(&root)
            .unwrap()
            .into_iter()
            .map(|c| c.name)
            .collect();
        assert_eq!(names, ["zeta", "alpha.txt", "Beta.txt"]);
    }
}

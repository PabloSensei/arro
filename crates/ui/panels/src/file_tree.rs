//! The project tree as data: what was read from disk, what is expanded and the
//! flat list of visible rows. No UI and no I/O on the calling thread, except
//! [`read_folder`], which the panel runs in the background.

use std::collections::{BTreeSet, HashMap, HashSet};
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
    /// The folder or file the row acts on. For a compacted chain it is the deepest folder.
    pub path: PathBuf,
    /// The first folder of the row. It is `path` unless the row is a compacted chain.
    pub head: PathBuf,
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
    /// Folders whose rows are set from outside the disk, such as the migrations.
    virtual_folders: HashSet<PathBuf>,
    expanded: BTreeSet<PathBuf>,
    /// Whether a folder whose only child is a folder shares its row with that child.
    compact: bool,
    /// The source roots of the project model. A package below one of them is named with dots.
    source_roots: Vec<PathBuf>,
    /// Folders whose listing the rows need and that have none yet.
    missing: Vec<PathBuf>,
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
            virtual_folders: HashSet::new(),
            expanded,
            compact: false,
            source_roots: Vec::new(),
            missing: Vec::new(),
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

    pub fn is_expanded(&self, folder: &Path) -> bool {
        self.expanded.contains(folder)
    }

    /// Folders whose listing the rows need and do not have yet.
    pub fn pending(&self) -> Vec<PathBuf> {
        self.missing.clone()
    }

    /// The folders the panel reads again after a change on disk: the open folders, and in
    /// compact mode every folder a row stands for, so that a chain changed on disk is found.
    pub fn listed_folders(&self) -> Vec<PathBuf> {
        let mut folders = Vec::new();
        for row in self.rows.iter().filter(|row| row.folder) {
            if self.compact && !self.is_virtual_child(&row.path) {
                folders.extend(self.members(row));
            } else if row.open {
                folders.push(row.path.clone());
            }
        }
        folders.retain(|folder| !self.virtual_folders.contains(folder));
        folders
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

    /// Adds a row to `parent` that is not read from disk, or gives the row with the same
    /// name new contents, such as the node of the migrations of a project.
    pub fn set_virtual_child(&mut self, parent: &Path, child: Child) {
        let children = self.virtuals.entry(parent.to_path_buf()).or_default();
        children.retain(|known| known.name != child.name);
        children.push(child);
        self.rebuild();
    }

    /// Takes away the row of `parent` with that name, if the tree has it.
    pub fn remove_virtual_child(&mut self, parent: &Path, name: &str) {
        let Some(children) = self.virtuals.get_mut(parent) else {
            return;
        };
        children.retain(|child| child.name != name);
        if children.is_empty() {
            self.virtuals.remove(parent);
        }
        self.rebuild();
    }

    /// Sets the rows of a folder whose files live elsewhere, such as the root of the
    /// migrations: the tree must not read that folder from disk.
    pub fn set_virtual_listing(&mut self, folder: PathBuf, children: Vec<Child>) {
        self.virtual_folders.insert(folder.clone());
        self.listings.insert(folder, children);
        self.rebuild();
    }

    /// Forgets the rows of a folder of [`FileTree::set_virtual_listing`].
    pub fn clear_virtual_listing(&mut self, folder: &Path) {
        self.virtual_folders.remove(folder);
        self.listings.remove(folder);
        self.rebuild();
    }

    /// Whether the rows of a folder come from outside the disk.
    pub fn is_virtual_folder(&self, path: &Path) -> bool {
        self.virtual_folders.contains(path)
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

    /// Shows every chain of folders that have one subfolder each as one row, as IDEA does
    /// with the middle packages, or every folder as a row of its own.
    pub fn set_compact(&mut self, on: bool) {
        if self.compact != on {
            self.compact = on;
            self.rebuild();
        }
    }

    pub fn compact(&self) -> bool {
        self.compact
    }

    /// Sets the source roots of the project model. Returns whether they changed.
    pub fn set_source_roots(&mut self, roots: Vec<PathBuf>) -> bool {
        if self.source_roots == roots {
            return false;
        }
        self.source_roots = roots;
        self.rebuild();
        true
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

    /// The row that shows `path`: its own row, or the compacted row that holds it.
    pub fn index_of(&self, path: &Path) -> Option<usize> {
        self.rows.iter().position(|row| {
            row.path == path || (path.starts_with(&row.head) && row.path.starts_with(path))
        })
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
            head: self.root.clone(),
            name: name.into_owned(),
            depth: 0,
            folder: true,
            open: self.expanded.contains(&self.root),
        }];
        let mut missing = Vec::new();
        if rows[0].open {
            if !self.listings.contains_key(&self.root) {
                want(&mut missing, &self.root);
            }
            self.append_children(&self.root, 1, &mut rows, &mut missing);
        }
        self.rows = rows;
        self.missing = missing;
    }

    /// Adds the rows of the children of `folder`. Virtual rows stay after the real ones, so
    /// that a folder of the project keeps the place the keys of the tree expect.
    fn append_children(
        &self,
        folder: &Path,
        depth: usize,
        rows: &mut Vec<Row>,
        missing: &mut Vec<PathBuf>,
    ) {
        for child in self.listings.get(folder).into_iter().flatten() {
            self.push_child(child, false, depth, rows, missing);
        }
        for child in self.virtuals.get(folder).into_iter().flatten() {
            self.push_child(child, true, depth, rows, missing);
        }
    }

    /// Adds the row of one child: a compacted chain of folders when the tree shows them so,
    /// otherwise the child as it is.
    fn push_child(
        &self,
        child: &Child,
        virtual_row: bool,
        depth: usize,
        rows: &mut Vec<Row>,
        missing: &mut Vec<PathBuf>,
    ) {
        let compactable =
            self.compact && !virtual_row && child.folder && !self.is_source_root(&child.path);
        if compactable && let Some(tail) = self.chain_tail(&child.path, missing) {
            let separator = if self.is_under_source_root(&child.path) {
                "."
            } else {
                "/"
            };
            let name = chain_names(&child.path, &tail).join(separator);
            let open = self.expanded.contains(&tail);
            rows.push(Row {
                path: tail.clone(),
                head: child.path.clone(),
                name,
                depth,
                folder: true,
                open,
            });
            if open {
                self.append_children(&tail, depth + 1, rows, missing);
            }
            return;
        }
        let open = child.folder && self.expanded.contains(&child.path);
        if open && !self.listings.contains_key(&child.path) {
            want(missing, &child.path);
        }
        rows.push(Row {
            path: child.path.clone(),
            head: child.path.clone(),
            name: child.name.clone(),
            depth,
            folder: child.folder,
            open,
        });
        if open {
            self.append_children(&child.path, depth + 1, rows, missing);
        }
    }

    /// The last folder of the chain that starts at `head`: every folder in it has exactly
    /// one child and that child is a folder, and a source root never joins a chain. `None`
    /// when a listing on the way is not known yet, and the folder whose listing is missing
    /// is asked for.
    fn chain_tail(&self, head: &Path, missing: &mut Vec<PathBuf>) -> Option<PathBuf> {
        let mut tail = head.to_path_buf();
        loop {
            let Some(children) = self.listings.get(&tail) else {
                want(missing, &tail);
                return None;
            };
            // A folder with a virtual row has more than one child, however the disk looks.
            if self
                .virtuals
                .get(&tail)
                .is_some_and(|virtual_rows| !virtual_rows.is_empty())
            {
                return Some(tail);
            }
            let [only] = children.as_slice() else {
                return Some(tail);
            };
            if !only.folder || self.is_source_root(&only.path) {
                return Some(tail);
            }
            tail = only.path.clone();
        }
    }

    /// The folders a row stands for, from its head down to its path.
    fn members(&self, row: &Row) -> Vec<PathBuf> {
        let mut members: Vec<PathBuf> = row
            .path
            .ancestors()
            .take_while(|folder| folder.starts_with(&row.head))
            .map(Path::to_path_buf)
            .collect();
        members.reverse();
        members
    }

    fn is_virtual_child(&self, path: &Path) -> bool {
        self.virtuals
            .values()
            .flatten()
            .any(|child| child.path == path)
    }

    fn is_source_root(&self, folder: &Path) -> bool {
        self.source_roots.iter().any(|root| root == folder)
    }

    /// Whether a folder lies below a source root, where its packages are named with dots.
    fn is_under_source_root(&self, folder: &Path) -> bool {
        folder
            .ancestors()
            .skip(1)
            .any(|ancestor| self.is_source_root(ancestor))
    }
}

/// The names of the folders from `head` down to `tail`, as the row shows them.
fn chain_names(head: &Path, tail: &Path) -> Vec<String> {
    let mut names: Vec<String> = tail
        .ancestors()
        .take_while(|folder| folder.starts_with(head))
        .map(|folder| {
            folder
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.reverse();
    names
}

/// Asks for the listing of `folder` once, however many rows need it.
fn want(missing: &mut Vec<PathBuf>, folder: &Path) {
    if !missing.iter().any(|known| known == folder) {
        missing.push(folder.to_path_buf());
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
    fn a_chain_of_single_subfolders_is_one_row_named_with_slashes() {
        let mut tree = FileTree::new(PathBuf::from("/p"), []);
        tree.set_compact(true);
        tree.set_listing(PathBuf::from("/p"), vec![child("/p/a", true)]);
        tree.set_listing(PathBuf::from("/p/a"), vec![child("/p/a/b", true)]);
        tree.set_listing(PathBuf::from("/p/a/b"), vec![child("/p/a/b/c", true)]);
        tree.set_listing(
            PathBuf::from("/p/a/b/c"),
            vec![child("/p/a/b/c/x.txt", false)],
        );
        assert_eq!(names(&tree), [(0, "p"), (1, "a/b/c")]);
        assert_eq!(tree.rows()[1].path, PathBuf::from("/p/a/b/c"));
        assert_eq!(tree.rows()[1].head, PathBuf::from("/p/a"));

        // Every folder of the chain finds the one row that shows it.
        assert_eq!(tree.index_of(Path::new("/p/a")), Some(1));
        assert_eq!(tree.index_of(Path::new("/p/a/b")), Some(1));
        assert_eq!(tree.index_of(Path::new("/p/a/b/c")), Some(1));

        tree.toggle(Path::new("/p/a/b/c"));
        assert_eq!(names(&tree), [(0, "p"), (1, "a/b/c"), (2, "x.txt")]);
    }

    #[test]
    fn a_folder_below_a_source_root_is_named_with_dots_and_the_root_stays_a_row() {
        let mut tree = FileTree::new(PathBuf::from("/p"), []);
        tree.set_compact(true);
        tree.set_source_roots(vec![PathBuf::from("/p/src")]);
        tree.set_listing(PathBuf::from("/p"), vec![child("/p/src", true)]);
        tree.toggle(Path::new("/p/src"));
        tree.set_listing(PathBuf::from("/p/src"), vec![child("/p/src/com", true)]);
        tree.set_listing(
            PathBuf::from("/p/src/com"),
            vec![child("/p/src/com/demo", true)],
        );
        tree.set_listing(
            PathBuf::from("/p/src/com/demo"),
            vec![child("/p/src/com/demo/App.txt", false)],
        );
        // The source root is a row of its own; the chain below it starts after it.
        assert_eq!(names(&tree), [(0, "p"), (1, "src"), (2, "com.demo")]);
        tree.toggle(Path::new("/p/src/com/demo"));
        assert_eq!(
            names(&tree),
            [(0, "p"), (1, "src"), (2, "com.demo"), (3, "App.txt")]
        );
    }

    #[test]
    fn a_folder_with_a_file_or_two_folders_ends_its_chain() {
        let mut tree = FileTree::new(PathBuf::from("/p"), []);
        tree.set_compact(true);
        tree.set_listing(
            PathBuf::from("/p"),
            vec![child("/p/a", true), child("/p/b", true)],
        );
        tree.set_listing(PathBuf::from("/p/a"), vec![child("/p/a/c", true)]);
        tree.set_listing(
            PathBuf::from("/p/a/c"),
            vec![
                child("/p/a/c/one.txt", false),
                child("/p/a/c/two.txt", false),
            ],
        );
        tree.set_listing(PathBuf::from("/p/b"), Vec::new());
        assert_eq!(names(&tree), [(0, "p"), (1, "a/c"), (1, "b")]);
    }

    #[test]
    fn a_chain_waits_for_the_listings_it_needs_and_asks_for_them() {
        let mut tree = FileTree::new(PathBuf::from("/p"), []);
        tree.set_compact(true);
        tree.set_listing(PathBuf::from("/p"), vec![child("/p/a", true)]);
        // The chain is not known until the folder below `a` is read.
        assert_eq!(names(&tree), [(0, "p"), (1, "a")]);
        assert_eq!(tree.pending(), [PathBuf::from("/p/a")]);
        assert_eq!(
            tree.listed_folders(),
            [PathBuf::from("/p"), PathBuf::from("/p/a")]
        );
        tree.set_listing(PathBuf::from("/p/a"), vec![child("/p/a/b", true)]);
        assert_eq!(tree.pending(), [PathBuf::from("/p/a/b")]);
    }

    #[test]
    fn a_folder_that_is_not_open_is_asked_for_only_in_compact_mode() {
        let mut tree = sample();
        assert!(tree.pending().is_empty());
        tree.set_compact(true);
        assert_eq!(tree.pending(), [PathBuf::from("/p/src")]);
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

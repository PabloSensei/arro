//! The tree of changed files that the Commit and Push windows draw. The folders of the
//! paths are rows above their files, and a folder that holds one folder and no file is
//! folded into the row of that folder, as IDEA folds a chain of packages: `src/main/acme`
//! is one row. The windows add the icons, the ticks and the clicks; this module knows no
//! window and no language.

use std::collections::{BTreeMap, HashSet};

/// What a row of the tree is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NodeKind {
    /// A repository of the project, when the project has submodules.
    Root,
    /// The heading of one state of the changes: Changes or Unversioned Files.
    Group,
    /// The heading of one module, when the files are grouped by module.
    Module,
    /// A folder, or the chain of folders folded into one row.
    Folder,
    /// A file.
    File,
}

/// One row of the tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TreeNode {
    pub(crate) kind: NodeKind,
    /// The number of rows the row sits under, which sets its indent.
    pub(crate) depth: usize,
    /// The key of the row. The collapsed set holds the keys of the rows that hide their
    /// rows below, and two rows of one list never share a key.
    pub(crate) key: String,
    /// The text of the row: the name of a file, or the names of a folder chain joined by
    /// slashes.
    pub(crate) label: String,
    /// The path of the folder or the file, relative to the root of the project; empty for
    /// a heading.
    pub(crate) path: String,
    /// The index of the file in the list the tree was built from.
    pub(crate) file: Option<usize>,
    /// The indices of the files the row holds, in the order of the list.
    pub(crate) files: Vec<usize>,
    /// Whether the rows below a folder or a heading are drawn.
    pub(crate) open: bool,
}

/// The key of the row of a folder. The scope keeps the keys of two trees apart, so two
/// lists of files may both have a folder `src`.
pub(crate) fn folder_key(scope: &str, path: &str) -> String {
    format!("dir:{scope}:{path}")
}

/// The key of the row of a file, unique in one list of files.
pub(crate) fn file_key(index: usize) -> String {
    format!("file:{index}")
}

/// The indent of one level of rows, in pixels, as the tree rows of the other windows have it.
pub(crate) const INDENT: f32 = 19.;

/// The icon of a folder row, as the project tree and the Changed files tree draw it.
pub(crate) const FOLDER_ICON: &str = "nodes/folder";

/// The icon of a module heading.
pub(crate) const MODULE_ICON: &str = "nodes/module";

/// The row of a heading over files: a repository, a state of the changes or a module.
pub(crate) fn heading(
    kind: NodeKind,
    depth: usize,
    key: String,
    label: String,
    path: String,
    files: Vec<usize>,
    open: bool,
) -> TreeNode {
    TreeNode {
        kind,
        depth,
        key,
        label,
        path,
        file: None,
        files,
        open,
    }
}

/// The count of a heading as the window writes it: `1 file`, `3 files`.
pub(crate) fn file_count(count: usize) -> String {
    if count == 1 {
        "1 file".to_owned()
    } else {
        format!("{count} files")
    }
}

/// The rows of the tree of `files`, given as `(path, index)` pairs. Folders come before the
/// files of their level, and both are sorted by name. `scope` tells this tree from the
/// others, `depth` is the depth of the top rows, and a folder whose key is in `collapsed`
/// has no rows below it.
pub(crate) fn tree_rows(
    files: &[(&str, usize)],
    scope: &str,
    depth: usize,
    collapsed: &HashSet<String>,
) -> Vec<TreeNode> {
    let mut root = Folder::default();
    for (path, index) in files {
        let parts: Vec<&str> = path.split('/').collect();
        root.insert(&parts, *index);
    }
    let mut rows = Vec::new();
    emit(&root, scope, "", depth, collapsed, &mut rows);
    rows
}

/// A folder of paths: the folders below it and the files directly in it.
#[derive(Default)]
struct Folder {
    folders: BTreeMap<String, Folder>,
    files: BTreeMap<String, usize>,
}

impl Folder {
    fn insert(&mut self, parts: &[&str], index: usize) {
        match parts {
            [] => {}
            [name] => {
                self.files.insert((*name).to_owned(), index);
            }
            [head, rest @ ..] => {
                self.folders
                    .entry((*head).to_owned())
                    .or_default()
                    .insert(rest, index);
            }
        }
    }

    /// The indices of all the files below this folder, in ascending order.
    fn all_files(&self) -> Vec<usize> {
        let mut indices: Vec<usize> = self.files.values().copied().collect();
        for folder in self.folders.values() {
            indices.extend(folder.all_files());
        }
        indices.sort_unstable();
        indices
    }
}

/// Appends the rows of the folder `folder`, whose path is `base`.
fn emit(
    folder: &Folder,
    scope: &str,
    base: &str,
    depth: usize,
    collapsed: &HashSet<String>,
    rows: &mut Vec<TreeNode>,
) {
    for (name, child) in &folder.folders {
        let mut label = name.clone();
        let mut path = join(base, name);
        let mut node = child;
        // A folder that holds one folder and no file folds that folder into its own row.
        while node.files.is_empty() && node.folders.len() == 1 {
            let Some((next, below)) = node.folders.iter().next() else {
                break;
            };
            label.push('/');
            label.push_str(next);
            path.push('/');
            path.push_str(next);
            node = below;
        }
        let key = folder_key(scope, &path);
        let open = !collapsed.contains(&key);
        rows.push(TreeNode {
            kind: NodeKind::Folder,
            depth,
            key,
            label,
            path: path.clone(),
            file: None,
            files: node.all_files(),
            open,
        });
        if open {
            emit(node, scope, &path, depth + 1, collapsed, rows);
        }
    }
    for (name, index) in &folder.files {
        rows.push(TreeNode {
            kind: NodeKind::File,
            depth,
            key: file_key(*index),
            label: name.clone(),
            path: join(base, name),
            file: Some(*index),
            files: vec![*index],
            open: false,
        });
    }
}

fn join(base: &str, name: &str) -> String {
    if base.is_empty() {
        name.to_owned()
    } else {
        format!("{base}/{name}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(rows: &[TreeNode]) -> Vec<(String, usize)> {
        rows.iter()
            .map(|row| (row.label.clone(), row.depth))
            .collect()
    }

    #[test]
    fn a_chain_of_single_folders_is_one_row() {
        let files = [
            ("src/main/acme/owner/Owner.txt", 0),
            ("src/main/acme/owner/Pet.txt", 1),
            ("README.md", 2),
        ];
        let rows = tree_rows(&files, "changes", 0, &HashSet::new());
        assert_eq!(
            labels(&rows),
            vec![
                ("src/main/acme/owner".to_owned(), 0),
                ("Owner.txt".to_owned(), 1),
                ("Pet.txt".to_owned(), 1),
                ("README.md".to_owned(), 0),
            ],
            "the folders with one folder inside them fold into the row of the last one"
        );
        assert_eq!(rows[0].path, "src/main/acme/owner");
        assert_eq!(rows[0].files, vec![0, 1]);
        assert_eq!(rows[1].file, Some(0));
        assert_eq!(rows[3].files, vec![2]);
    }

    #[test]
    fn a_folder_that_holds_a_file_is_not_folded() {
        let files = [("src/Main.txt", 0), ("src/util/Strings.txt", 1)];
        let rows = tree_rows(&files, "changes", 0, &HashSet::new());
        assert_eq!(
            labels(&rows),
            vec![
                ("src".to_owned(), 0),
                ("util".to_owned(), 1),
                ("Strings.txt".to_owned(), 2),
                ("Main.txt".to_owned(), 1),
            ],
            "src holds Main.txt, so it is not folded into util or the other way round"
        );
        assert_eq!(rows[0].files, vec![0, 1]);
    }

    #[test]
    fn folders_come_before_the_files_of_their_level_and_both_are_sorted() {
        let files = [
            ("zeta.txt", 0),
            ("alpha/one.txt", 1),
            ("beta/two.txt", 2),
            ("alpha/sub/three.txt", 3),
        ];
        let rows = tree_rows(&files, "changes", 0, &HashSet::new());
        assert_eq!(
            labels(&rows),
            vec![
                ("alpha".to_owned(), 0),
                ("sub".to_owned(), 1),
                ("three.txt".to_owned(), 2),
                ("one.txt".to_owned(), 1),
                ("beta".to_owned(), 0),
                ("two.txt".to_owned(), 1),
                ("zeta.txt".to_owned(), 0),
            ]
        );
    }

    #[test]
    fn a_collapsed_folder_hides_the_rows_below_it() {
        let files = [("src/main/acme/owner/Owner.txt", 0), ("README.md", 1)];
        let collapsed = HashSet::from([folder_key("changes", "src/main/acme/owner")]);
        let rows = tree_rows(&files, "changes", 0, &collapsed);
        assert_eq!(
            labels(&rows),
            vec![
                ("src/main/acme/owner".to_owned(), 0),
                ("README.md".to_owned(), 0),
            ]
        );
        assert!(!rows[0].open, "the folded row is closed");
        assert_eq!(
            rows[0].files,
            vec![0],
            "a closed folder still holds its files"
        );
    }

    #[test]
    fn two_scopes_never_share_a_key() {
        assert_ne!(
            folder_key("changes", "src"),
            folder_key("unversioned", "src")
        );
    }
}

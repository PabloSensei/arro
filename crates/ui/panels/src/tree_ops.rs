//! What the project tree does to files: create, rename, delete. The planning is pure and
//! the disk work blocks, so the panel runs [`create_on_disk`] and the others in the
//! background.

use std::io::Write;
use std::path::{Path, PathBuf};

/// What a template makes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Creates {
    File,
    /// A folder. With `dotted` a name like `a.b.c` is three folders inside each other.
    Folder {
        dotted: bool,
    },
}

/// A kind of thing the project tree can create. The application gives the list: the
/// neutral crates know no languages.
#[derive(Clone)]
pub struct FileTemplate {
    pub label: &'static str,
    pub icon: &'static str,
    /// Added to a name that has no extension. Empty for any file.
    pub extension: &'static str,
    pub creates: Creates,
    /// The text of a new file, from the folder it goes into, the project root and its name
    /// without the extension.
    pub content: fn(&Path, &Path, &str) -> String,
}

/// The templates that need no application: an empty file and a plain folder.
pub fn basic_templates() -> Vec<FileTemplate> {
    vec![
        FileTemplate {
            label: "File",
            icon: "fileTypes/text",
            extension: "",
            creates: Creates::File,
            content: |_, _, _| String::new(),
        },
        FileTemplate {
            label: "Directory",
            icon: "nodes/folder",
            extension: "",
            creates: Creates::Folder { dotted: false },
            content: |_, _, _| String::new(),
        },
    ]
}

/// What one creation will put on disk.
#[derive(Debug, PartialEq, Eq)]
pub struct Creation {
    pub path: PathBuf,
    /// `None` for a folder.
    pub content: Option<String>,
}

const FORBIDDEN: &[char] = &['<', '>', ':', '"', '|', '?', '*'];

/// Whether `name` can be one file or folder name on Windows.
pub fn check_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name == "." || name == ".." {
        return Err("The name is empty".into());
    }
    if name.ends_with('.') || name.ends_with(' ') {
        return Err(format!("\"{name}\" ends with a dot or a space"));
    }
    if let Some(bad) = name
        .chars()
        .find(|c| FORBIDDEN.contains(c) || c.is_control())
    {
        return Err(format!("\"{name}\" has the character {bad:?}"));
    }
    Ok(())
}

/// What `input` asks for inside `folder`. The text of a file comes from the template.
pub fn plan_creation(
    template: &FileTemplate,
    folder: &Path,
    root: &Path,
    input: &str,
) -> Result<Creation, String> {
    plan_creation_with_template(template, folder, root, input, None)
}

/// [`plan_creation`] with the text of a new file from the templates of the user. Without
/// a definition the content function of the template makes the text.
pub fn plan_creation_with_template(
    template: &FileTemplate,
    folder: &Path,
    root: &Path,
    input: &str,
    definition: Option<&templates::FileTemplateDefinition>,
) -> Result<Creation, String> {
    let input = input.trim();
    match template.creates {
        Creates::File => {
            check_name(input)?;
            if input.contains(['/', '\\']) {
                return Err("A file name has no slashes".into());
            }
            let has_extension = Path::new(input).extension().is_some();
            let name = if has_extension || template.extension.is_empty() {
                input.to_string()
            } else {
                format!("{input}.{}", template.extension)
            };
            let stem = Path::new(&name)
                .file_stem()
                .map_or_else(String::new, |stem| stem.to_string_lossy().into_owned());
            check_name(&stem)?;
            let text = match definition {
                Some(definition) => definition.render(root, folder, &stem),
                None => (template.content)(folder, root, &stem),
            };
            Ok(Creation {
                path: folder.join(name),
                content: Some(text),
            })
        }
        Creates::Folder { dotted } => {
            let mut path = folder.to_path_buf();
            let parts = input.split(['/', '\\']);
            for part in parts.flat_map(|part| {
                let pieces: Vec<&str> = if dotted {
                    part.split('.').collect()
                } else {
                    vec![part]
                };
                pieces
            }) {
                check_name(part)?;
                path.push(part);
            }
            Ok(Creation {
                path,
                content: None,
            })
        }
    }
}

/// Puts the creation on disk. Never overwrites: an existing file is an error. Blocks.
pub fn create_on_disk(creation: &Creation) -> std::io::Result<()> {
    match &creation.content {
        Some(text) => {
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&creation.path)?;
            file.write_all(text.as_bytes())
        }
        None => {
            if creation.path.exists() {
                return Err(std::io::ErrorKind::AlreadyExists.into());
            }
            std::fs::create_dir_all(&creation.path)
        }
    }
}

/// The new place of `path` when it gets another name. Blocks on nothing.
pub fn renamed(path: &Path, new_name: &str) -> Result<PathBuf, String> {
    let new_name = new_name.trim();
    check_name(new_name)?;
    if new_name.contains(['/', '\\']) {
        return Err("A name has no slashes".into());
    }
    let parent = path
        .parent()
        .ok_or("The project folder cannot be renamed")?;
    Ok(parent.join(new_name))
}

/// Where a drag puts `from` when it is dropped on `target_folder`. `Ok(None)` means the
/// item is already there and nothing happens; `Err` says why the drop is refused.
pub fn plan_move(
    from: &Path,
    target_folder: &Path,
    folder: bool,
) -> Result<Option<PathBuf>, String> {
    if folder && target_folder == from {
        return Err("A folder cannot be moved into itself".into());
    }
    if folder && target_folder.starts_with(from) {
        return Err("A folder cannot be moved into its own folder".into());
    }
    if from.parent() == Some(target_folder) {
        return Ok(None);
    }
    let name = from
        .file_name()
        .ok_or("The project folder cannot be moved")?;
    let to = target_folder.join(name);
    if to.exists() {
        return Err(format!(
            "\"{}\" already exists in {}",
            name.to_string_lossy(),
            target_folder.display()
        ));
    }
    Ok(Some(to))
}

/// The name a copy gets when it lands next to a file of the same name: `Name_1.ext`,
/// `Name_2.ext`, as IDEA offers when something is copied into its own folder. A name
/// that already ends with a number, like `Name_1.ext`, gets the next number rather than
/// another suffix.
pub fn copy_name(name: &str, serial: usize) -> String {
    let path = Path::new(name);
    let (stem, extension) = match path.extension() {
        Some(extension) => (
            path.file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            format!(".{}", extension.to_string_lossy()),
        ),
        None => (name.to_owned(), String::new()),
    };
    let (base, current) = match stem.rsplit_once('_') {
        Some((base, number))
            if !base.is_empty()
                && !number.is_empty()
                && number.chars().all(|character| character.is_ascii_digit()) =>
        {
            (base.to_owned(), number.parse::<usize>().unwrap_or(0))
        }
        _ => (stem, 0),
    };
    format!("{base}_{}{extension}", current + serial)
}

/// Where a paste of `from` into `target_folder` goes. A copy beside its source gets a
/// numbered name; a cut beside its source does nothing. The existence of the place is
/// checked here, so the panel calls this away from the window thread.
pub fn plan_paste(from: &Path, target_folder: &Path, cut: bool) -> Result<Option<PathBuf>, String> {
    if from.parent() == Some(target_folder) {
        if cut {
            return Ok(None);
        }
        let name = from
            .file_name()
            .ok_or("The project folder cannot be pasted")?
            .to_string_lossy();
        for serial in 1..=10_000 {
            let to = target_folder.join(copy_name(&name, serial));
            if !to.exists() {
                return Ok(Some(to));
            }
        }
        return Err(format!(
            "\"{name}\" has too many copies in {}",
            target_folder.display()
        ));
    }
    if from.is_dir() {
        if target_folder == from {
            return Err("A folder cannot be copied into itself".into());
        }
        if target_folder.starts_with(from) {
            return Err("A folder cannot be copied into its own folder".into());
        }
    }
    let name = from
        .file_name()
        .ok_or("The project folder cannot be pasted")?;
    let to = target_folder.join(name);
    if to.exists() {
        return Err(format!(
            "\"{}\" already exists in {}",
            name.to_string_lossy(),
            target_folder.display()
        ));
    }
    Ok(Some(to))
}

/// Renames on disk. A name that is taken is an error, also when only the case differs
/// from another file. Blocks.
pub fn rename_on_disk(from: &Path, to: &Path) -> std::io::Result<()> {
    let same_file_new_case = from != to
        && from
            .to_string_lossy()
            .eq_ignore_ascii_case(&to.to_string_lossy());
    if to.exists() && !same_file_new_case {
        return Err(std::io::ErrorKind::AlreadyExists.into());
    }
    std::fs::rename(from, to)
}

/// Moves a file or a folder with everything in it to the Recycle Bin. Blocks.
pub fn delete_on_disk(path: &Path) -> std::io::Result<()> {
    #[cfg(test)]
    if let Some(delete) = DELETE_HOOK.get() {
        return delete(path);
    }
    platform::move_to_trash(path)
}

/// The tests of the window delete in a scratch folder instead of the Recycle Bin.
#[cfg(test)]
static DELETE_HOOK: std::sync::OnceLock<fn(&Path) -> std::io::Result<()>> =
    std::sync::OnceLock::new();

/// Makes every delete of this test binary take the file out of the way without the
/// Recycle Bin, so `cargo test` does not put scratch files there. Idempotent.
#[cfg(test)]
pub(crate) fn use_temporary_delete() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let _ = DELETE_HOOK.set(|path| {
            use std::sync::atomic::{AtomicU64, Ordering};
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let serial = NEXT.fetch_add(1, Ordering::Relaxed);
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            let target = std::env::temp_dir().join(format!(
                "ide-deleted-{}-{serial}-{name}",
                std::process::id()
            ));
            std::fs::rename(path, &target)?;
            if target.is_dir() {
                std::fs::remove_dir_all(&target)
            } else {
                std::fs::remove_file(&target)
            }
        });
    });
}

/// The folder a new item goes into: the selected folder, the folder of a selected file,
/// or the project folder when nothing is selected.
pub fn target_folder(root: &Path, selected: Option<(&Path, bool)>) -> PathBuf {
    match selected {
        Some((path, true)) => path.to_path_buf(),
        Some((path, false)) => path.parent().unwrap_or(root).to_path_buf(),
        None => root.to_path_buf(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn class() -> FileTemplate {
        FileTemplate {
            label: "Class",
            icon: "nodes/class",
            extension: "src",
            creates: Creates::File,
            content: |folder, _, name| format!("{} {name}", folder.display()),
        }
    }

    fn package() -> FileTemplate {
        FileTemplate {
            label: "Package",
            icon: "nodes/folder",
            extension: "",
            creates: Creates::Folder { dotted: true },
            content: |_, _, _| String::new(),
        }
    }

    #[test]
    fn a_file_gets_the_extension_of_its_template_unless_it_has_one() {
        let (folder, root) = (Path::new("/p/src"), Path::new("/p"));
        let made = plan_creation(&class(), folder, root, " Order ").unwrap();
        assert_eq!(made.path, Path::new("/p/src/Order.src"));
        assert_eq!(made.content.as_deref(), Some("/p/src Order"));

        let named = plan_creation(&class(), folder, root, "Order.txt").unwrap();
        assert_eq!(named.path, Path::new("/p/src/Order.txt"));
    }

    #[test]
    fn bad_names_are_refused_with_a_reason() {
        let (folder, root) = (Path::new("/p"), Path::new("/p"));
        for bad in ["", "..", "a/b", "a:b", "a?", "name.", "a*b"] {
            assert!(
                plan_creation(&class(), folder, root, bad).is_err(),
                "{bad:?} should be refused"
            );
        }
    }

    #[test]
    fn a_dotted_folder_is_nested_and_a_plain_one_takes_slashes() {
        let (folder, root) = (Path::new("/p/src"), Path::new("/p"));
        let nested = plan_creation(&package(), folder, root, "com.shop.api").unwrap();
        assert_eq!(nested.path, Path::new("/p/src/com/shop/api"));
        assert_eq!(nested.content, None);

        let plain = &basic_templates()[1];
        let path = plan_creation(plain, folder, root, "a/b.c").unwrap().path;
        assert_eq!(path, Path::new("/p/src/a/b.c"));
        assert!(plan_creation(&package(), folder, root, "com..shop").is_err());
    }

    #[test]
    fn the_target_follows_the_selection() {
        let root = Path::new("/p");
        assert_eq!(target_folder(root, None), Path::new("/p"));
        let folder = Some((Path::new("/p/src"), true));
        assert_eq!(target_folder(root, folder), Path::new("/p/src"));
        let file = Some((Path::new("/p/src/A.txt"), false));
        assert_eq!(target_folder(root, file), Path::new("/p/src"));
    }

    #[test]
    fn a_rename_keeps_the_folder_and_refuses_the_root() {
        let path = Path::new("/p/src/A.txt");
        assert_eq!(renamed(path, "B.txt").unwrap(), Path::new("/p/src/B.txt"));
        assert!(renamed(path, "x/y").is_err());
        assert!(renamed(Path::new("/"), "x").is_err());
    }

    #[test]
    fn a_drag_lands_in_the_target_folder_and_refuses_its_own_subfolder() {
        let src = Path::new("/p/src");
        let dest = Path::new("/p/src/dest");
        let file = Path::new("/p/src/A.txt");
        assert_eq!(
            plan_move(file, dest, false).unwrap(),
            Some(PathBuf::from("/p/src/dest/A.txt"))
        );
        assert_eq!(
            plan_move(file, src, false).unwrap(),
            None,
            "the file is already there"
        );
        assert!(plan_move(src, src, true).is_err());
        assert!(plan_move(src, dest, true).is_err());
        assert_eq!(
            plan_move(src, Path::new("/p"), true).unwrap(),
            None,
            "the folder is already there"
        );
        assert_eq!(
            plan_move(src, Path::new("/p/lib"), true).unwrap(),
            Some(PathBuf::from("/p/lib/src"))
        );
    }

    #[test]
    fn a_copy_beside_its_source_gets_a_numbered_name() {
        let root = std::env::temp_dir().join(format!("paste-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let source = root.join("Name.txt");
        std::fs::write(&source, "one").unwrap();

        assert_eq!(copy_name("Name.txt", 1), "Name_1.txt");
        assert_eq!(copy_name("Name.txt", 2), "Name_2.txt");
        assert_eq!(copy_name("folder", 1), "folder_1");
        assert_eq!(
            plan_paste(&source, &root, false).unwrap(),
            Some(root.join("Name_1.txt"))
        );
        std::fs::write(root.join("Name_1.txt"), "two").unwrap();
        assert_eq!(
            plan_paste(&source, &root, false).unwrap(),
            Some(root.join("Name_2.txt"))
        );
        assert_eq!(
            plan_paste(&source, &root, true).unwrap(),
            None,
            "a cut beside its source does nothing"
        );
        let other = root.join("other");
        std::fs::create_dir_all(&other).unwrap();
        assert_eq!(
            plan_paste(&source, &other, true).unwrap(),
            Some(other.join("Name.txt"))
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_numbered_name_gets_the_next_number_rather_than_another_suffix() {
        assert_eq!(
            copy_name("Name_1.txt", 1),
            "Name_2.txt",
            "the number grows rather than the suffix"
        );
        assert_eq!(copy_name("Name_1.txt", 2), "Name_3.txt");
        assert_eq!(copy_name("folder_4", 1), "folder_5");
    }

    #[test]
    fn files_are_created_renamed_and_deleted_on_disk_without_overwriting() {
        // The delete below must not put anything in the Recycle Bin of the machine.
        use_temporary_delete();
        let root = std::env::temp_dir().join(format!("tree-ops-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let template = &basic_templates()[0];
        let made = plan_creation(template, &root, &root, "A.txt").unwrap();
        create_on_disk(&made).unwrap();
        assert!(create_on_disk(&made).is_err(), "never overwrites");

        let folder = plan_creation(&package(), &root, &root, "x.y").unwrap();
        create_on_disk(&folder).unwrap();
        assert!(create_on_disk(&folder).is_err());

        let moved = renamed(&made.path, "B.txt").unwrap();
        rename_on_disk(&made.path, &moved).unwrap();
        assert!(moved.exists() && !made.path.exists());
        std::fs::write(root.join("C.txt"), "").unwrap();
        assert!(rename_on_disk(&moved, &root.join("C.txt")).is_err());

        delete_on_disk(&moved).unwrap();
        delete_on_disk(&root.join("x")).unwrap();
        assert!(!moved.exists() && !root.join("x").exists());
        std::fs::remove_dir_all(&root).unwrap();
    }
}

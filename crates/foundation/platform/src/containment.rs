//! Whether a path lies inside a folder, judged by the place it really names. The symbolic
//! links of the path are followed, so a link inside a project that points out of it makes
//! the path an outside one.

use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

/// The path that `path` names under `root`, when it lies inside `root`; `None` when it does
/// not.
///
/// The text of the path loses its `.` and `..` first. Then the part of the path that exists
/// is followed to the place the system opens it at, and that place must lie inside the
/// place `root` names. A part that does not exist yet is judged by the folder it is in, so a
/// new file is checked before it is written. A link that dangles is refused, because a write
/// through it would land wherever the link points.
///
/// The returned path is the cleaned text, spelled as the caller gave it, so the caller reads
/// or writes exactly the place that was checked. Case counts on Linux only; Windows and macOS
/// compare the names without case.
pub fn inside_root(root: &Path, path: &Path) -> Option<PathBuf> {
    let mut lexical = PathBuf::new();
    for component in root.join(path).components() {
        match component {
            Component::ParentDir => {
                // A `..` past the start of a relative path would reach a folder the cleaned
                // text cannot name, so the path is refused instead of being cut short.
                if !lexical.pop() && lexical.as_os_str().is_empty() {
                    return None;
                }
            }
            Component::CurDir => {}
            other => lexical.push(other),
        }
    }
    let real_root = real_location(&std::path::absolute(root).ok()?)?;
    let real = real_location(&std::path::absolute(&lexical).ok()?)?;
    comparable(&real)
        .starts_with(comparable(&real_root))
        .then_some(lexical)
}

/// The place `path` names with the links of its existing part followed. A part that does not
/// exist is kept by name after the folder that does. `None` when a part cannot be followed:
/// a link that dangles, a loop of links or an error of the system.
fn real_location(path: &Path) -> Option<PathBuf> {
    let mut missing = Vec::new();
    let mut current = path;
    loop {
        match fs::canonicalize(current) {
            Ok(real) => {
                let mut real = plain(real);
                for name in missing.iter().rev() {
                    real.push(name);
                }
                return Some(real);
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                // An entry that is there as a link, even a dangling one, is not judged by
                // its folder: the system would follow it on the next use.
                if fs::symlink_metadata(current).is_ok() {
                    return None;
                }
                missing.push(current.file_name()?.to_os_string());
                current = current.parent()?;
            }
            Err(_) => return None,
        }
    }
}

/// The form of a path that `fs::canonicalize` gives, without the verbatim prefix of
/// Windows, which the other paths do not carry.
fn plain(path: PathBuf) -> PathBuf {
    if cfg!(windows) {
        let text = path.to_string_lossy().into_owned();
        if let Some(share) = text.strip_prefix(r"\\?\UNC\") {
            return PathBuf::from(format!(r"\\{share}"));
        }
        if let Some(rest) = text.strip_prefix(r"\\?\") {
            return PathBuf::from(rest);
        }
    }
    path
}

/// The form in which two places of one system are compared: Windows and macOS ignore the
/// case of a name, the other systems compare it as it is.
fn comparable(path: &Path) -> PathBuf {
    if cfg!(any(windows, target_os = "macos")) {
        PathBuf::from(path.to_string_lossy().to_lowercase())
    } else {
        path.to_path_buf()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_support::TempDir;

    fn folder(name: &str) -> (TempDir, PathBuf) {
        let temp = TempDir::new(name);
        let project = temp.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        (temp, project)
    }

    #[test]
    fn a_path_of_the_folder_is_inside_it_even_when_it_does_not_exist() {
        let (_temp, project) = folder("containment-new");

        assert_eq!(
            inside_root(&project, Path::new("src/new/A.txt")),
            Some(project.join("src").join("new").join("A.txt"))
        );
        assert_eq!(
            inside_root(&project, Path::new("src/../A.txt")),
            Some(project.join("A.txt"))
        );
    }

    #[test]
    fn a_path_that_leaves_the_folder_is_outside_it() {
        let (_temp, project) = folder("containment-leave");

        assert_eq!(inside_root(&project, Path::new("../other.txt")), None);
        assert_eq!(
            inside_root(&project, Path::new("src/../../other.txt")),
            None
        );
        let beside = project.with_file_name("project-other").join("A.txt");
        assert_eq!(inside_root(&project, &beside), None);
        assert_eq!(
            inside_root(&project, &project.join("A.txt")),
            Some(project.join("A.txt")),
            "an absolute path inside the folder is inside"
        );
    }

    #[test]
    fn a_relative_folder_is_judged_as_it_is_spelled() {
        assert_eq!(
            inside_root(Path::new("work/project"), Path::new("src/../a.txt")),
            Some(PathBuf::from("work/project/a.txt"))
        );
        assert_eq!(
            inside_root(Path::new("work/project"), Path::new("../secret")),
            None
        );
        assert_eq!(
            inside_root(Path::new("work/project"), Path::new("../../../secret")),
            None,
            "a `..` past the start of the relative path is refused, not cut short"
        );
    }

    #[test]
    fn a_link_inside_the_folder_to_a_place_outside_it_is_outside() {
        let (temp, project) = folder("containment-link-out");
        let outside = temp.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret.txt"), "key").unwrap();
        if test_support::link_dir(&outside, &project.join("docs")).is_err() {
            return;
        }

        assert_eq!(inside_root(&project, Path::new("docs/secret.txt")), None);
        assert_eq!(inside_root(&project, Path::new("docs/new.txt")), None);
    }

    #[test]
    fn a_link_inside_the_folder_to_a_place_inside_it_is_inside() {
        let (_temp, project) = folder("containment-link-in");
        std::fs::create_dir_all(project.join("src")).unwrap();
        if test_support::link_dir(&project.join("src"), &project.join("alias")).is_err() {
            return;
        }

        assert_eq!(
            inside_root(&project, Path::new("alias/A.txt")),
            Some(project.join("alias").join("A.txt"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_dangling_link_is_outside_even_when_its_name_is_inside() {
        let (temp, project) = folder("containment-dangling");
        let missing = temp.path().join("missing");
        std::os::unix::fs::symlink(&missing, project.join("docs")).unwrap();

        assert_eq!(inside_root(&project, Path::new("docs/A.txt")), None);
        assert_eq!(inside_root(&project, Path::new("docs")), None);
    }

    #[test]
    fn a_name_that_differs_in_case_is_the_same_place_only_where_case_does_not_count() {
        let (_temp, project) = folder("containment-case");
        let upper = project.with_file_name(
            project
                .file_name()
                .unwrap()
                .to_string_lossy()
                .to_uppercase(),
        );
        let path = upper.join("A.txt");

        let inside = inside_root(&project, &path).is_some();
        assert_eq!(inside, cfg!(any(windows, target_os = "macos")));
    }
}

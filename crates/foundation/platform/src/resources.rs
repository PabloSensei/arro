//! Where a program keeps the files it ships with: next to its executable, or in the
//! `Resources` folder of a macOS application bundle.
//!
//! The layout is read from the path alone, so a test of another system can check the
//! bundle of macOS without the bundle being there.

use std::path::{Path, PathBuf};

/// The folder of the files of the program at `program`: `Contents/Resources` when the
/// program lies in `<name>.app/Contents/MacOS`, the folder of the program everywhere else.
pub fn resources_dir(program: &Path) -> PathBuf {
    program
        .parent()
        .map_or_else(|| program.to_owned(), resources_dir_of)
}

/// [`resources_dir`] for a program that lies in `folder`.
pub fn resources_dir_of(folder: &Path) -> PathBuf {
    if folder.file_name().is_some_and(|name| name == "MacOS")
        && let Some(contents) = folder.parent()
        && contents.file_name().is_some_and(|name| name == "Contents")
        && contents.parent().is_some_and(|bundle| {
            bundle
                .extension()
                .is_some_and(|extension| extension == "app")
        })
    {
        return contents.join("Resources");
    }
    folder.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bundle_keeps_its_files_under_contents() {
        assert_eq!(
            resources_dir(Path::new("/Applications/Arro.app/Contents/MacOS/arro")),
            Path::new("/Applications/Arro.app/Contents/Resources")
        );
    }

    #[test]
    fn a_plain_program_keeps_its_files_next_to_itself() {
        assert_eq!(
            resources_dir(Path::new("/opt/arro/arro")),
            Path::new("/opt/arro")
        );
    }

    #[test]
    fn a_windows_program_keeps_its_files_next_to_itself() {
        // Slashes keep the path readable on every system; Windows splits on both.
        assert_eq!(
            resources_dir(Path::new("C:/Program Files/Arro/arro.exe")),
            Path::new("C:/Program Files/Arro")
        );
        #[cfg(windows)]
        assert_eq!(
            resources_dir(Path::new(r"C:\Program Files\Arro\arro.exe")),
            Path::new(r"C:\Program Files\Arro")
        );
    }

    #[test]
    fn only_the_layout_of_a_bundle_counts() {
        assert_eq!(
            resources_dir(Path::new("/opt/Arro/Contents/MacOS/arro")),
            Path::new("/opt/Arro/Contents/MacOS")
        );
        assert_eq!(
            resources_dir(Path::new("/opt/Other.app/Contents/Helpers/arro")),
            Path::new("/opt/Other.app/Contents/Helpers")
        );
    }
}

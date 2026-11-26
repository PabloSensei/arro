//! The permission bits a release archive carries. Windows has no such bits, so every
//! function is a no-op there and the files keep the attributes they came with.

use std::io;
use std::path::Path;

/// Sets the bits of `mode` on `path`, as a tar or zip header of a Unix system gives
/// them. A zero mode means the archive did not name one.
#[cfg(unix)]
pub fn set_mode(path: &Path, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    if mode == 0 {
        return Ok(());
    }
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode & 0o777))
}

#[cfg(not(unix))]
pub fn set_mode(_path: &Path, _mode: u32) -> io::Result<()> {
    Ok(())
}

/// Adds the execute bits of the owner, group and others to `path`.
#[cfg(unix)]
pub fn make_executable(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = std::fs::metadata(path)?.permissions();
    permissions.set_mode(permissions.mode() | 0o755);
    std::fs::set_permissions(path, permissions)
}

#[cfg(not(unix))]
pub fn make_executable(_path: &Path) -> io::Result<()> {
    Ok(())
}

/// Creates or replaces a file that its owner alone may read and write: `0600` on a
/// system with permission bits, the attributes of the folder on Windows.
pub fn private_file(path: &Path) -> io::Result<std::fs::File> {
    #[cfg(unix)]
    let file = {
        use std::os::unix::fs::OpenOptionsExt;

        std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?
    };
    #[cfg(not(unix))]
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)?;
    // The mode of an existing file is kept by `open`, so it is set again.
    set_mode(path, 0o600)?;
    Ok(file)
}

/// Creates a folder that its owner alone may enter: `0700` on a system with permission bits,
/// the attributes of the folder on Windows. The folders above it are made as usual, and an
/// existing folder gets the same mode.
pub fn private_dir(path: &Path) -> io::Result<()> {
    std::fs::create_dir_all(path)?;
    set_mode(path, 0o700)
}

/// Creates one folder that its owner alone may enter: `0700` on a system with permission
/// bits, the attributes of the folder on Windows. It fails when `path` exists, so a folder
/// another user made under the same name is never taken over.
#[cfg(unix)]
pub fn private_new_dir(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;

    std::fs::DirBuilder::new().mode(0o700).create(path)
}

/// Creates one folder that its owner alone may enter, see the Unix version.
#[cfg(not(unix))]
pub fn private_new_dir(path: &Path) -> io::Result<()> {
    std::fs::create_dir(path)
}

/// The permission bits of `path`, or `0` on a system without them.
#[cfg(unix)]
pub fn mode(path: &Path) -> io::Result<u32> {
    use std::os::unix::fs::PermissionsExt;

    Ok(std::fs::metadata(path)?.permissions().mode() & 0o777)
}

#[cfg(not(unix))]
pub fn mode(_path: &Path) -> io::Result<u32> {
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_private_folder_is_made_with_its_parents_and_keeps_its_mode() {
        let temp = test_support::TempDir::new("private-dir");
        let folder = temp.path().join("config").join("ide");

        private_dir(&folder).expect("the folder is made");
        private_dir(&folder).expect("a second call changes nothing");

        assert!(folder.is_dir());
        if cfg!(unix) {
            assert_eq!(mode(&folder).expect("the mode"), 0o700);
        }
    }
}

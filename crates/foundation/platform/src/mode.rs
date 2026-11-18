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

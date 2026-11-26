//! Links between names on disk. A test that needs a real symbolic link has to make one
//! through the system API, so the call lives here rather than in the test.

use std::io;
use std::path::Path;

/// Creates a symbolic link at `link` that names the file `target`.
#[cfg(unix)]
pub fn symlink_file(target: &Path, link: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

/// Creates a symbolic link at `link` that names the file `target`. Windows needs the
/// right to make symbolic links; a system that refuses hands the error back.
#[cfg(windows)]
pub fn symlink_file(target: &Path, link: &Path) -> io::Result<()> {
    std::os::windows::fs::symlink_file(target, link)
}

/// Creates a symbolic link at `link` that names the file `target`.
#[cfg(not(any(unix, windows)))]
pub fn symlink_file(_target: &Path, _link: &Path) -> io::Result<()> {
    Err(io::Error::other("this system has no symbolic links"))
}

//! Reading and writing a document. These block, so call them away from the UI thread.

use std::fs;
use std::io::{self, ErrorKind, Write};
use std::path::{Path, PathBuf};

use crate::encoding::{Decoded, FileFormat, decode, encode};

/// Larger files open read-only, as in IDEA.
pub const MAX_EDITABLE_BYTES: u64 = 20 * 1024 * 1024;

pub fn read_file(path: &Path) -> io::Result<Decoded> {
    let bytes = fs::read(path)?;
    let mut decoded = decode(&bytes);
    decoded.lossless &= bytes.len() as u64 <= MAX_EDITABLE_BYTES;
    Ok(decoded)
}

/// Replaces the file in one step: a crash leaves the old content or the new one.
pub fn write_file(path: &Path, text: &str, format: &FileFormat) -> io::Result<()> {
    let bytes = encode(text, format);
    let temporary = temporary_path(path)?;
    let result = write_and_replace(&temporary, path, &bytes);
    if result.is_err() {
        remove(&temporary);
    }
    result
}

/// The new contents of one file in [`write_files`].
pub struct FileUpdate<'a> {
    pub path: &'a Path,
    /// Text with `\n` line ends, as the buffers keep it.
    pub text: &'a str,
    pub format: &'a FileFormat,
}

/// Replaces several files in one step: either all of them change or none do. Every
/// new text goes to a temporary file next to its target first, in the target's own
/// encoding and line ends; only when all of them are written do the temporaries
/// replace the targets. A file that cannot be written leaves every original untouched,
/// and the error names it.
///
/// `std::fs::rename` replaces an existing file on Windows (`MoveFileExW` with
/// `MOVEFILE_REPLACE_EXISTING`), so this needs no Win32 calls.
pub fn write_files(files: &[FileUpdate<'_>]) -> io::Result<()> {
    // A read-only file or one held open without write access fails here, before any
    // temporary exists and before any target is replaced.
    for file in files {
        if let Err(error) = check_replaceable(file.path) {
            return Err(with_path(file.path, error));
        }
    }
    let mut staged = Vec::with_capacity(files.len());
    for (index, file) in files.iter().enumerate() {
        match stage(file, index) {
            Ok(temporary) => staged.push((temporary, file.path)),
            Err(error) => {
                for (temporary, _) in &staged {
                    remove(temporary);
                }
                return Err(with_path(file.path, error));
            }
        }
    }
    commit(&staged)
}

fn temporary_path(path: &Path) -> io::Result<PathBuf> {
    let file_name = path
        .file_name()
        .ok_or_else(|| io::Error::other("no file name"))?;
    Ok(path.with_file_name(format!(
        ".{}.{}.tmp",
        file_name.to_string_lossy(),
        std::process::id()
    )))
}

/// A name of our own next to the target; the index keeps files of one batch apart.
fn staged_path(path: &Path, index: usize) -> io::Result<PathBuf> {
    let file_name = path
        .file_name()
        .ok_or_else(|| io::Error::other("no file name"))?;
    Ok(path.with_file_name(format!(
        ".{}.{}.{}.tmp",
        file_name.to_string_lossy(),
        std::process::id(),
        index
    )))
}

fn backup_path(path: &Path, index: usize) -> io::Result<PathBuf> {
    let file_name = path
        .file_name()
        .ok_or_else(|| io::Error::other("no file name"))?;
    Ok(path.with_file_name(format!(
        ".{}.{}.{}.bak",
        file_name.to_string_lossy(),
        std::process::id(),
        index
    )))
}

fn stage(file: &FileUpdate<'_>, index: usize) -> io::Result<PathBuf> {
    let bytes = encode(file.text, file.format);
    let temporary = staged_path(file.path, index)?;
    let written = (|| {
        let mut handle = fs::File::create(&temporary)?;
        handle.write_all(&bytes)?;
        handle.sync_all()?;
        drop(handle);
        if let Ok(metadata) = fs::metadata(file.path) {
            fs::set_permissions(&temporary, metadata.permissions())?;
        }
        Ok(())
    })();
    if let Err(error) = written {
        remove(&temporary);
        return Err(error);
    }
    Ok(temporary)
}

fn check_replaceable(path: &Path) -> io::Result<()> {
    let metadata = fs::metadata(path)?;
    if metadata.permissions().readonly() {
        return Err(io::Error::new(
            ErrorKind::PermissionDenied,
            "the file is read-only",
        ));
    }
    // A file held open without write access refuses this, and the rename would fail.
    fs::OpenOptions::new().write(true).open(path)?;
    Ok(())
}

/// The target moves aside, the temporary takes its place; a failure puts the target
/// back, so the batch leaves the earlier files as they were too.
fn commit(staged: &[(PathBuf, &Path)]) -> io::Result<()> {
    let mut replaced: Vec<(PathBuf, PathBuf)> = Vec::with_capacity(staged.len());
    for (index, (temporary, path)) in staged.iter().enumerate() {
        let backup = match backup_path(path, index) {
            Ok(backup) => backup,
            Err(error) => {
                roll_back(&replaced);
                remove_staged(staged);
                return Err(with_path(path, error));
            }
        };
        let moved = fs::rename(path, &backup).and_then(|()| fs::rename(temporary, path));
        match moved {
            Ok(()) => replaced.push((backup, path.to_path_buf())),
            Err(error) => {
                if backup.exists() && !path.exists() {
                    let _ = fs::rename(&backup, path);
                }
                remove(&backup);
                roll_back(&replaced);
                remove_staged(staged);
                return Err(with_path(path, error));
            }
        }
    }
    for (backup, _) in replaced {
        remove(&backup);
    }
    Ok(())
}

fn roll_back(replaced: &[(PathBuf, PathBuf)]) {
    for (backup, path) in replaced.iter().rev() {
        let _ = fs::rename(backup, path);
    }
}

fn remove_staged(staged: &[(PathBuf, &Path)]) {
    for (temporary, _) in staged {
        remove(temporary);
    }
}

fn remove(path: &Path) {
    let _ = fs::remove_file(path);
}

fn with_path(path: &Path, error: io::Error) -> io::Error {
    io::Error::new(error.kind(), format!("{}: {error}", path.display()))
}

fn write_and_replace(temporary: &Path, path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = fs::File::create(temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    if let Ok(metadata) = fs::metadata(path) {
        fs::set_permissions(temporary, metadata.permissions())?;
    }
    fs::rename(temporary, path)
}

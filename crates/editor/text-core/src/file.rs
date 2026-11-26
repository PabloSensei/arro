//! Reading and writing a document. These block, so call them away from the UI thread.

use std::collections::HashMap;
use std::fs;
use std::io::{self, ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::encoding::{Decoded, FileFormat, decode, encode_checked};

/// Larger files open read-only, as in IDEA.
pub const MAX_EDITABLE_BYTES: u64 = 20 * 1024 * 1024;

/// Names the temporary files of this process; with the pid it keeps two saves of one path
/// apart, and the time keeps two processes apart.
static TEMPORARY_SERIAL: AtomicU64 = AtomicU64::new(0);

pub fn read_file(path: &Path) -> io::Result<Decoded> {
    let bytes = fs::read(path)?;
    let mut decoded = decode(&bytes);
    decoded.lossless &= bytes.len() as u64 <= MAX_EDITABLE_BYTES;
    Ok(decoded)
}

/// Replaces the file in one step: a crash leaves the old content or the new one.
///
/// A target without write access is refused on every system. On Windows the rename
/// fails on its own, but on Unix only the directory needs the write permission, so a
/// rename would silently replace a read-only file.
///
/// A symbolic link is resolved first, so the link stays a link and its target changes.
/// Saves of one path wait for each other, so two of them never share a temporary file
/// and the file always holds one whole snapshot.
pub fn write_file(path: &Path, text: &str, format: &FileFormat) -> io::Result<()> {
    let target = write_target(path);
    let guard = lock_for(&target);
    let _held = guard
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Err(error) = check_writable(&target) {
        return Err(with_path(path, error));
    }
    let (bytes, unrepresentable) = encode_checked(text, format);
    if unrepresentable {
        return Err(with_path(path, cannot_store(format)));
    }
    let temporary = temporary_path(&target)?;
    let result = write_and_replace(&temporary, &target, &bytes);
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
    let targets: Vec<PathBuf> = files.iter().map(|file| write_target(file.path)).collect();
    // Locks are taken in one order for every batch, so two batches that touch the same
    // paths never wait for each other in a circle.
    let mut keys = targets.clone();
    keys.sort();
    keys.dedup();
    let locks: Vec<Arc<Mutex<()>>> = keys.iter().map(|key| lock_for(key)).collect();
    let _held: Vec<_> = locks
        .iter()
        .map(|lock| lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner()))
        .collect();
    // A read-only file or one held open without write access fails here, before any
    // temporary exists and before any target is replaced.
    for (file, target) in files.iter().zip(&targets) {
        if let Err(error) = check_replaceable(target) {
            return Err(with_path(file.path, error));
        }
    }
    let mut staged = Vec::with_capacity(files.len());
    for (index, (file, target)) in files.iter().zip(&targets).enumerate() {
        match stage(file, target, index) {
            Ok(temporary) => staged.push((temporary, target.clone())),
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

/// The file a save writes: the target of a symbolic link, so the link itself stays. A
/// link with a missing target keeps its own name; that save makes the target.
fn write_target(path: &Path) -> PathBuf {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
        }
        _ => path.to_path_buf(),
    }
}

/// The locks of the paths a save is writing.
static PATH_LOCKS: OnceLock<Mutex<HashMap<PathBuf, Weak<Mutex<()>>>>> = OnceLock::new();

/// The lock of one path, shared by every save of it in this process.
fn lock_for(path: &Path) -> Arc<Mutex<()>> {
    let locks = PATH_LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut paths = locks
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(lock) = paths.get(path).and_then(Weak::upgrade) {
        return lock;
    }
    let lock = Arc::new(Mutex::new(()));
    paths.insert(path.to_path_buf(), Arc::downgrade(&lock));
    lock
}

fn cannot_store(format: &FileFormat) -> io::Error {
    io::Error::new(
        ErrorKind::InvalidData,
        format!(
            "the text contains characters that {} cannot store",
            format.encoding.name()
        ),
    )
}

fn temporary_path(path: &Path) -> io::Result<PathBuf> {
    let file_name = path
        .file_name()
        .ok_or_else(|| io::Error::other("no file name"))?;
    Ok(path.with_file_name(format!(
        ".{}.{}.tmp",
        file_name.to_string_lossy(),
        temporary_suffix()
    )))
}

/// A part of a name nothing else of this process uses, even when two saves start at once.
fn temporary_suffix() -> String {
    let serial = TEMPORARY_SERIAL.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.subsec_nanos())
        .unwrap_or(0);
    format!("{}.{serial:x}.{nanos:x}", std::process::id())
}

/// A name of our own next to the target; the index keeps files of one batch apart.
fn staged_path(path: &Path, index: usize) -> io::Result<PathBuf> {
    let file_name = path
        .file_name()
        .ok_or_else(|| io::Error::other("no file name"))?;
    Ok(path.with_file_name(format!(
        ".{}.{}.{}.tmp",
        file_name.to_string_lossy(),
        temporary_suffix(),
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
        temporary_suffix(),
        index
    )))
}

fn stage(file: &FileUpdate<'_>, target: &Path, index: usize) -> io::Result<PathBuf> {
    let (bytes, unrepresentable) = encode_checked(file.text, file.format);
    if unrepresentable {
        return Err(cannot_store(file.format));
    }
    let temporary = staged_path(target, index)?;
    let written = (|| {
        let mut handle = fs::File::create(&temporary)?;
        handle.write_all(&bytes)?;
        handle.sync_all()?;
        drop(handle);
        if let Ok(metadata) = fs::metadata(target) {
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
    check_permissions(path, &metadata)
}

/// The check of [`write_file`], where a missing target is a file about to be created.
fn check_writable(path: &Path) -> io::Result<()> {
    match fs::metadata(path) {
        Ok(metadata) => check_permissions(path, &metadata),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn check_permissions(path: &Path, metadata: &fs::Metadata) -> io::Result<()> {
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
fn commit(staged: &[(PathBuf, PathBuf)]) -> io::Result<()> {
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

fn remove_staged(staged: &[(PathBuf, PathBuf)]) {
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

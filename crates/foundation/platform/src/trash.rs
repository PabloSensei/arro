//! The Recycle Bin of Windows and the trash of the desktop: a delete the user can take back.

use std::io;
use std::path::Path;
#[cfg(unix)]
use std::path::PathBuf;

/// Moves a file or a whole folder to the Recycle Bin. Blocks: call it off the UI thread.
#[cfg(windows)]
pub fn move_to_trash(path: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;

    use windows_sys::Win32::UI::Shell::{
        FO_DELETE, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT, SHFILEOPSTRUCTW,
        SHFileOperationW,
    };

    let mut from: Vec<u16> = path.as_os_str().encode_wide().collect();
    from.push(0);
    from.push(0);
    let mut operation = SHFILEOPSTRUCTW {
        wFunc: FO_DELETE,
        pFrom: from.as_ptr(),
        fFlags: (FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_NOERRORUI | FOF_SILENT) as u16,
        ..Default::default()
    };
    // The call copies the path before it returns, so `from` stays alive for the whole call.
    let result = unsafe { SHFileOperationW(&mut operation) };
    if result == 0 && operation.fAnyOperationsAborted == 0 {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(result as i32))
    }
}

/// Moves a file or a whole folder to the trash of the desktop: the `files` folder keeps
/// the entry, the `info` folder keeps where it came from. Blocks: call it off the UI thread.
#[cfg(all(unix, not(target_os = "macos")))]
pub fn move_to_trash(path: &Path) -> io::Result<()> {
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local").join("share"))
        })
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no home folder for a trash"))?;
    move_to_trash_in(path, &data.join("Trash"))
}

/// Moves a file or a whole folder to `$HOME/.Trash`, which is the trash Finder shows.
/// Blocks: call it off the UI thread.
#[cfg(target_os = "macos")]
pub fn move_to_trash(path: &Path) -> io::Result<()> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no home folder for a trash"))?;
    move_into_trash(path, &home.join(".Trash"))
}

/// A trash folder of its own, the way the Trash of the Finder holds an entry under its
/// name and renames it when the name is taken.
#[cfg(target_os = "macos")]
fn move_into_trash(path: &Path, trash: &Path) -> io::Result<()> {
    let name = path.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} has no name to put into the trash", path.display()),
        )
    })?;
    std::fs::create_dir_all(trash)?;

    // Finder writes `Report 2.pdf` next to `Report.pdf`; the first free number wins.
    let mut target = trash.join(name);
    let mut counter = 2;
    while target.exists() {
        let numbered = format!("{} {counter}", name.to_string_lossy());
        target = trash.join(numbered);
        counter += 1;
    }
    if std::fs::rename(path, &target).is_err() {
        // The trash of another volume needs a copy; a failed copy leaves no half entry.
        copy_then_remove(path, &target)?;
    }
    Ok(())
}

/// The freedesktop Trash specification, against an explicit trash folder.
#[cfg(all(unix, not(target_os = "macos")))]
fn move_to_trash_in(path: &Path, trash: &Path) -> io::Result<()> {
    let name = path.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} has no name to put into the trash", path.display()),
        )
    })?;
    let files = trash.join("files");
    let info = trash.join("info");
    std::fs::create_dir_all(&files)?;
    std::fs::create_dir_all(&info)?;

    // Another entry with the same name keeps its place; the new one gets a suffix.
    let mut target = files.join(name);
    let mut entry = info.join(format!("{}.trashinfo", name.to_string_lossy()));
    let mut counter = 1;
    while target.exists() || entry.exists() {
        let numbered = format!("{}.{counter}", name.to_string_lossy());
        target = files.join(&numbered);
        entry = info.join(format!("{numbered}.trashinfo"));
        counter += 1;
    }

    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut content = String::from("[Trash Info]\nPath=");
    percent_encode(&absolute.to_string_lossy(), &mut content);
    content.push_str("\nDeletionDate=");
    content.push_str(&timestamp());
    content.push('\n');
    std::fs::write(&entry, content)?;
    if let Err(error) = std::fs::rename(path, &target) {
        // A trash on another disk needs a copy; a failed copy leaves no half entry.
        if let Err(copy) = copy_then_remove(path, &target) {
            let _ = std::fs::remove_file(&entry);
            return Err(io::Error::new(
                error.kind(),
                format!("{}: {error}; the copy failed too: {copy}", path.display()),
            ));
        }
    }
    Ok(())
}

#[cfg(all(unix, not(target_os = "macos")))]
fn percent_encode(text: &str, out: &mut String) {
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' => out.push(byte as char),
            b'-' | b'_' | b'.' | b'~' | b'/' | b':' => out.push(byte as char),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
fn timestamp() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};

    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs()) as i64;
    let days = seconds / 86_400;
    let remaining = seconds % 86_400;
    let (hour, minute, second) = (remaining / 3600, remaining % 3600 / 60, remaining % 60);
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}")
}

/// Days since 1970-01-01 to a calendar date, the same arithmetic as the build script.
#[cfg(all(unix, not(target_os = "macos")))]
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { year + 1 } else { year };
    (year, month as u32, day as u32)
}

#[cfg(unix)]
fn copy_then_remove(source: &Path, target: &Path) -> io::Result<()> {
    let metadata = std::fs::symlink_metadata(source)?;
    if metadata.is_dir() {
        std::fs::create_dir_all(target)?;
        for entry in std::fs::read_dir(source)? {
            let entry = entry?;
            copy_then_remove(&entry.path(), &target.join(entry.file_name()))?;
        }
        std::fs::remove_dir(source)
    } else {
        std::fs::copy(source, target)?;
        std::fs::remove_file(source)
    }
}

#[cfg(test)]
mod tests {
    #[cfg(target_os = "macos")]
    use super::move_into_trash;
    #[cfg(windows)]
    use super::move_to_trash;
    #[cfg(all(unix, not(target_os = "macos")))]
    use super::move_to_trash_in;

    /// Deliberately puts one temporary file into the real Recycle Bin: this is the only
    /// test of the binary that does, so every `cargo test` checks the Win32 call itself.
    #[cfg(windows)]
    #[test]
    fn a_file_goes_to_the_recycle_bin() {
        let _guard = crate::WIN32_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let dir = test_support::scratch("trash-test");
        let file = dir.join("taken-back.txt");
        std::fs::write(&file, "text").unwrap();

        move_to_trash(&file).unwrap();

        assert!(!file.exists(), "the file left the folder");
    }

    /// A trash folder of its own keeps the test away from the real trash of the user.
    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn a_file_gets_an_entry_of_the_freedesktop_trash() {
        let dir = std::env::temp_dir().join(format!("trash-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("taken-back.txt");
        std::fs::write(&file, "text").unwrap();
        let trash = dir.join("Trash");

        move_to_trash_in(&file, &trash).unwrap();

        assert!(!file.exists(), "the file left the folder");
        assert_eq!(
            std::fs::read_to_string(trash.join("files/taken-back.txt")).unwrap(),
            "text"
        );
        let info = std::fs::read_to_string(trash.join("info/taken-back.txt.trashinfo")).unwrap();
        assert!(info.starts_with("[Trash Info]\nPath="), "{info}");
        assert!(info.contains("DeletionDate="), "{info}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The Trash of the Finder keeps the file under its name and renames a second one.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_file_goes_into_the_trash_of_the_finder() {
        let dir = test_support::scratch("trash-test");
        let trash = dir.join(".Trash");
        let first = dir.join("taken-back.txt");
        std::fs::write(&first, "one").unwrap();

        move_into_trash(&first, &trash).unwrap();

        assert!(!first.exists(), "the file left the folder");
        assert_eq!(
            std::fs::read_to_string(trash.join("taken-back.txt")).unwrap(),
            "one"
        );
        let second = dir.join("taken-back.txt");
        std::fs::write(&second, "two").unwrap();
        move_into_trash(&second, &trash).unwrap();
        assert_eq!(
            std::fs::read_to_string(trash.join("taken-back 2.txt")).unwrap(),
            "two"
        );
    }
}

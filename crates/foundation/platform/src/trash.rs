//! The Recycle Bin of Windows: a delete that the user can take back.

use std::io;
use std::path::Path;

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

/// Other systems have no Recycle Bin; the caller has to ask before deleting for good.
#[cfg(not(windows))]
pub fn move_to_trash(_: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "the Recycle Bin is only on Windows",
    ))
}

#[cfg(test)]
mod tests {
    use super::move_to_trash;

    /// Deliberately puts one temporary file into the real Recycle Bin: this is the only
    /// test of the binary that does, so every `cargo test` checks the Win32 call itself.
    #[test]
    fn a_file_goes_to_the_recycle_bin() {
        let _guard = crate::WIN32_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let dir = std::env::temp_dir().join(format!("trash-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("taken-back.txt");
        std::fs::write(&file, "text").unwrap();

        move_to_trash(&file).unwrap();

        assert!(!file.exists(), "the file left the folder");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

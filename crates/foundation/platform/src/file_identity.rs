//! Whether two paths name one file on disk, as the file system sees it: a rename that only
//! changes case must not look like a clash with another file.

use std::fs::OpenOptions;
use std::io;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use std::path::Path;

use windows_sys::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, FILE_FLAG_BACKUP_SEMANTICS, FILE_READ_ATTRIBUTES,
    FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, GetFileInformationByHandle,
};

/// Whether `a` and `b` are one file: the same volume and the same file index. Both must exist.
/// A link is followed, as the path is opened.
pub fn same_file(a: &Path, b: &Path) -> io::Result<bool> {
    Ok(file_index(a)? == file_index(b)?)
}

/// The volume serial number and the file index of the file at `path`. They stay the same for
/// as long as the file exists, whatever its name.
fn file_index(path: &Path) -> io::Result<(u32, u64)> {
    // Reading the information needs no more than the right to read attributes. A folder opens
    // only with backup semantics. The standard library opens a long path as well.
    let file = OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)?;
    // SAFETY: an all-zero `BY_HANDLE_FILE_INFORMATION` is a valid value of plain integers; the
    // call fills it in.
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: `file` is open for the whole call and `info` is a valid place for the information.
    let read = unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) };
    if read == 0 {
        return Err(io::Error::last_os_error());
    }
    let index = (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow);
    Ok((info.dwVolumeSerialNumber, index))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::same_file;

    #[test]
    fn a_hard_link_is_the_same_file_and_a_copy_is_not() {
        let dir = test_support::scratch("file-identity-link");
        let first = dir.join("first.txt");
        let copy = dir.join("copy.txt");
        let link = dir.join("link.txt");
        fs::write(&first, "text").unwrap();
        fs::write(&copy, "text").unwrap();
        fs::hard_link(&first, &link).unwrap();

        assert!(same_file(&first, &link).unwrap());
        assert!(!same_file(&first, &copy).unwrap());
    }

    /// The name that differs only in case is the same file where the system ignores case,
    /// and another file where it does not.
    #[test]
    fn a_name_that_differs_only_in_case_is_the_same_file_only_where_case_is_ignored() {
        let dir = test_support::scratch("file-identity-case");
        let lower = dir.join("a.txt");
        fs::write(&lower, "one").unwrap();
        let upper = dir.join("A.txt");
        if upper.exists() {
            assert!(same_file(&lower, &upper).unwrap());
        } else {
            fs::write(&upper, "two").unwrap();
            assert!(!same_file(&lower, &upper).unwrap());
        }
    }

    #[test]
    fn a_missing_file_is_an_error() {
        let dir = test_support::scratch("file-identity-missing");
        fs::write(dir.join("here.txt"), "").unwrap();

        assert!(same_file(&dir.join("here.txt"), &dir.join("gone.txt")).is_err());
    }
}

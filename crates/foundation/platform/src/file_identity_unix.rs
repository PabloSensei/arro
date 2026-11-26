//! Whether two paths name one file on disk, as the file system sees it: a rename that only
//! changes case must not look like a clash with another file.

use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

/// Whether `a` and `b` are one file: the same device and the same inode. Both must exist.
/// A link is followed, as the path is opened.
pub fn same_file(a: &Path, b: &Path) -> io::Result<bool> {
    let (a, b) = (std::fs::metadata(a)?, std::fs::metadata(b)?);
    Ok(a.dev() == b.dev() && a.ino() == b.ino())
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

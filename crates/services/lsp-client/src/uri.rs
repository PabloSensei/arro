//! `file:` URIs. A server echoes these back, so one file must have one spelling:
//! every URI we send comes from a `CanonPath`.

use std::path::PathBuf;
use std::str::FromStr;

use lsp_types::Uri;
use platform::{CanonPath, path_from_uri};

pub fn file_uri(path: &CanonPath) -> Option<Uri> {
    Uri::from_str(&path.to_uri()).ok()
}

/// The path of a URI from a server, `None` for any scheme but `file:`.
pub fn file_path(uri: &Uri) -> Option<PathBuf> {
    path_from_uri(uri.as_str())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    fn uri(path: &str) -> String {
        file_uri(&CanonPath::new(path).unwrap())
            .unwrap()
            .as_str()
            .to_owned()
    }

    fn path(uri: &str) -> PathBuf {
        file_path(&Uri::from_str(uri).unwrap()).unwrap()
    }

    #[cfg(windows)]
    #[test]
    fn a_canonical_path_becomes_a_valid_uri() {
        assert_eq!(uri(r"z:\missing\.\a b.txt"), "file:///Z:/missing/a%20b.txt");
    }

    #[cfg(windows)]
    #[test]
    fn drive_letters_are_upper_case_and_percent_decoded() {
        assert_eq!(path("file:///c:/src/A.txt"), Path::new(r"C:\src\A.txt"));
        assert_eq!(path("file:///c%3A/src/A.txt"), Path::new(r"C:\src\A.txt"));
    }

    #[cfg(windows)]
    #[test]
    fn a_path_survives_the_way_there_and_back() {
        let text = r"Z:\Проекты\a b\c#d+e.txt";
        assert_eq!(path(&uri(text)), Path::new(text));
    }

    #[cfg(unix)]
    #[test]
    fn a_path_survives_the_way_there_and_back() {
        let text = "/home/user/Проекты/a b/c#d+e.txt";
        assert_eq!(path(&uri(text)), Path::new(text));
    }

    #[cfg(windows)]
    #[test]
    fn unc_paths_use_the_authority() {
        assert_eq!(
            path("file://server/share/a.txt"),
            Path::new(r"\\server\share\a.txt")
        );
    }

    #[test]
    fn other_schemes_have_no_path() {
        assert!(file_path(&Uri::from_str("custom://contents/A.class").unwrap()).is_none());
    }
}

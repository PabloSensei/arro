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

/// A `file:` URI a server sent, in the spelling Arro uses. A server writes the path the
/// way `File.toURI` spells it: raw non-ASCII characters, parentheses and `+` stay as they
/// are, which a URI parser refuses. The text therefore goes through the path and back.
/// `None` for a URI that is neither a file path nor valid URI text.
pub fn server_uri(text: &str) -> Option<Uri> {
    if let Some(path) = path_from_uri(text) {
        return Uri::from_str(&platform::path_to_uri(&path)).ok();
    }
    Uri::from_str(text).ok()
}

/// The key two spellings of one document URI share. A `file:` URI is reduced to the
/// normalized path of the file, so `file:/C:/Dir/A.txt` from a server and
/// `file:///C:/Dir/A.txt` from Arro are one document; any other URI is its text.
pub fn server_uri_key(text: &str) -> String {
    match path_from_uri(text) {
        Some(path) => platform::path_key(&path).to_string_lossy().into_owned(),
        None => text.to_owned(),
    }
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

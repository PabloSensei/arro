//! One spelling for one file on Unix: the absolute path that `canonicalize` resolves.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

/// One spelling for one file: absolute, with symbolic links of the existing part resolved.
/// Two equal paths are one file and a language server gets the URI it expects.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CanonPath {
    path: PathBuf,
}

impl CanonPath {
    /// A file that does not exist yet keeps the folder spelling it was given.
    pub fn new(path: impl AsRef<Path>) -> io::Result<Self> {
        let path = path.as_ref();
        let full = match std::fs::canonicalize(path) {
            Ok(full) => full,
            Err(_) => std::path::absolute(path)?,
        };
        Ok(Self { path: full })
    }

    pub fn as_path(&self) -> &Path {
        &self.path
    }

    /// `file:///home/user/a%20b.txt`.
    pub fn to_uri(&self) -> String {
        path_to_uri(&self.path)
    }
}

/// The `file:` URI of `path`, spelled the way [`CanonPath::to_uri`] spells it.
/// Percent-encodes every byte outside the set RFC 3986 allows unencoded, so a path with
/// spaces, parentheses, `+`, `@` or non-ASCII characters gives an ASCII URI.
pub fn path_to_uri(path: &Path) -> String {
    let text = path.to_string_lossy();
    let mut uri = String::from("file://");
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' => uri.push(byte as char),
            b'-' | b'_' | b'.' | b'~' | b'/' | b':' => uri.push(byte as char),
            _ => uri.push_str(&format!("%{byte:02X}")),
        }
    }
    uri
}

impl fmt::Display for CanonPath {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        self.path.display().fmt(f)
    }
}

/// Whether `path` names a share of another machine. Unix mounts a remote folder at a local
/// path and hides the difference, so only the UNC spelling is known to be remote.
pub fn is_remote_path(path: &Path) -> bool {
    let text = path.to_string_lossy();
    text.starts_with("//") || text.starts_with("\\\\")
}

/// The form two paths that name one file share on this system: macOS ignores case, the
/// other systems compare the spelling as it is.
#[cfg(target_os = "macos")]
pub fn path_key(path: &Path) -> PathBuf {
    PathBuf::from(path.to_string_lossy().to_lowercase())
}

#[cfg(not(target_os = "macos"))]
pub fn path_key(path: &Path) -> PathBuf {
    path.to_path_buf()
}

/// The path a `file:` URI names, the reverse of [`CanonPath::to_uri`]:
/// `file:///home/user/a%20b.txt`. `None` for any other scheme. A URI that names a host
/// keeps it in the path, because a path of Unix starts at the root.
pub fn path_from_uri(uri: &str) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStringExt;

    let rest = crate::url::file_rest(uri)?;
    let mut bytes = crate::url::percent_decode(rest)?;
    if !bytes.starts_with(b"/") {
        bytes.insert(0, b'/');
    }
    Some(PathBuf::from(std::ffi::OsString::from_vec(bytes)))
}

/// A `file://` URL as the Finder hands it over, the same path [`path_from_uri`] gives.
pub fn file_url_path(url: &str) -> Option<PathBuf> {
    path_from_uri(url)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uri_of(path: &str) -> String {
        CanonPath {
            path: PathBuf::from(path),
        }
        .to_uri()
    }

    #[test]
    fn uris_are_percent_encoded_as_utf8() {
        assert_eq!(uri_of("/home/user/a b.txt"), "file:///home/user/a%20b.txt");
        assert_eq!(
            uri_of("/home/user/проект/A.txt"),
            "file:///home/user/%D0%BF%D1%80%D0%BE%D0%B5%D0%BA%D1%82/A.txt"
        );
    }

    #[test]
    fn an_existing_file_has_one_spelling_however_it_is_named() {
        let directory = test_support::TempDir::new("canon-path");
        let file = directory.join("Name.txt");
        std::fs::write(&file, "x").unwrap();

        let exact = CanonPath::new(&file).unwrap();
        let other = CanonPath::new(directory.join(".").join("Name.txt")).unwrap();

        assert_eq!(exact, other);
    }

    #[test]
    fn a_new_file_gets_an_absolute_path() {
        let path = CanonPath::new("no-such-file.txt").unwrap();

        assert!(path.as_path().is_absolute());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn paths_that_differ_in_case_name_one_file() {
        assert_eq!(
            path_key(Path::new("/home/user/README.md")),
            path_key(Path::new("/home/user/readme.md"))
        );
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn paths_that_differ_in_case_are_two_files() {
        assert_ne!(
            path_key(Path::new("/home/user/README.md")),
            path_key(Path::new("/home/user/readme.md"))
        );
    }

    #[test]
    fn a_file_uri_is_a_path() {
        assert_eq!(
            path_from_uri("file:///home/user/a%20b.txt"),
            Some(PathBuf::from("/home/user/a b.txt"))
        );
        assert_eq!(
            path_from_uri("file://localhost/home/user/%D0%BF%D1%80/A.txt"),
            Some(PathBuf::from("/home/user/пр/A.txt"))
        );
        assert_eq!(
            path_from_uri("file://host/home/user/A.txt"),
            Some(PathBuf::from("/host/home/user/A.txt"))
        );
        assert_eq!(path_from_uri("https://example.com/a"), None);
        assert_eq!(path_from_uri("lib://contents/core/Text.class"), None);
    }

    #[test]
    fn a_path_survives_the_way_to_a_uri_and_back() {
        for path in [
            "/home/user/A.txt",
            "/home/user/My Files/a b.txt",
            "/home/user/проект/A.txt",
        ] {
            let uri = uri_of(path);
            assert_eq!(path_from_uri(&uri), Some(PathBuf::from(path)), "{uri}");
        }
    }

    #[test]
    fn a_path_from_a_uri_starts_at_the_root() {
        let path = path_from_uri("file:///tmp/a%20b.txt").unwrap();
        assert!(path.is_absolute(), "{}", path.display());
        assert_eq!(path, PathBuf::from("/tmp/a b.txt"));
    }
}

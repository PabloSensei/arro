use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

/// One spelling for one file: absolute, as the disk spells it when the file exists, with
/// an upper-case drive letter and without the `\\?\` prefix. Two equal paths are one file
/// and a language server gets the URI it expects.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CanonPath {
    path: PathBuf,
}

impl CanonPath {
    /// A file that does not exist yet keeps the case it was given.
    pub fn new(path: impl AsRef<Path>) -> io::Result<Self> {
        let path = path.as_ref();
        let full = match std::fs::canonicalize(path) {
            Ok(full) => full,
            Err(_) => std::path::absolute(path)?,
        };
        Ok(Self {
            path: PathBuf::from(normal_form(&full.to_string_lossy())),
        })
    }

    pub fn as_path(&self) -> &Path {
        &self.path
    }

    /// `file:///C:/dir/a%20b.txt` or `file://server/share/a.txt`.
    pub fn to_uri(&self) -> String {
        let text = self.path.to_string_lossy().replace('\\', "/");
        let (prefix, rest) = match text.strip_prefix("//") {
            Some(rest) => ("file://", rest),
            None => ("file:///", text.as_str()),
        };
        let mut uri = String::from(prefix);
        for byte in rest.bytes() {
            match byte {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' => uri.push(byte as char),
                b'-' | b'_' | b'.' | b'~' | b'/' | b':' => uri.push(byte as char),
                _ => uri.push_str(&format!("%{byte:02X}")),
            }
        }
        uri
    }
}

impl fmt::Display for CanonPath {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        self.path.display().fmt(f)
    }
}

/// The form two paths that name one file share on this system: Windows ignores case, so
/// `README.md` and `readme.md` of one folder give one key.
pub fn path_key(path: &Path) -> PathBuf {
    PathBuf::from(normal_form(&path.to_string_lossy()).to_lowercase())
}

fn normal_form(path: &str) -> String {
    let plain = match path.strip_prefix(r"\\?\UNC\") {
        Some(share) => format!(r"\\{share}"),
        None => path.strip_prefix(r"\\?\").unwrap_or(path).to_owned(),
    };
    let mut characters = plain.chars();
    match (characters.next(), characters.next()) {
        (Some(drive), Some(':')) => {
            format!("{}:{}", drive.to_ascii_uppercase(), characters.as_str())
        }
        _ => plain,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uri_of(path: &str) -> String {
        let path = PathBuf::from(normal_form(path));
        CanonPath { path }.to_uri()
    }

    #[test]
    fn one_spelling_for_drive_and_verbatim_paths() {
        assert_eq!(normal_form(r"c:\dir\a.txt"), r"C:\dir\a.txt");
        assert_eq!(normal_form(r"\\?\c:\dir\a.txt"), r"C:\dir\a.txt");
        assert_eq!(
            normal_form(r"\\?\UNC\server\share\a.txt"),
            r"\\server\share\a.txt"
        );
        assert_eq!(
            normal_form(r"\\server\share\a.txt"),
            r"\\server\share\a.txt"
        );
    }

    #[test]
    fn uris_are_percent_encoded_as_utf8() {
        assert_eq!(uri_of(r"c:\dir\a b.txt"), "file:///C:/dir/a%20b.txt");
        assert_eq!(
            uri_of(r"C:\проект\A.txt"),
            "file:///C:/%D0%BF%D1%80%D0%BE%D0%B5%D0%BA%D1%82/A.txt"
        );
        assert_eq!(uri_of(r"\\server\share\a.txt"), "file://server/share/a.txt");
    }

    #[test]
    fn an_existing_file_has_one_spelling_however_it_is_named() {
        let directory = test_support::scratch("canon-path");
        let file = directory.join("Name.txt");
        std::fs::write(&file, "x").unwrap();

        let exact = CanonPath::new(&file).unwrap();
        let other = CanonPath::new(directory.join(".").join("NAME.TXT")).unwrap();

        assert_eq!(exact, other);
        assert!(!exact.to_string().starts_with(r"\\?\"));
    }

    #[test]
    fn a_new_file_gets_an_absolute_path() {
        let path = CanonPath::new("no-such-file.txt").unwrap();

        assert!(path.as_path().is_absolute());
    }

    #[test]
    fn paths_that_differ_in_case_name_one_file() {
        assert_eq!(
            path_key(Path::new(r"c:\dir\README.md")),
            path_key(Path::new(r"C:\DIR\readme.md"))
        );
        assert_eq!(
            path_key(Path::new(r"\\?\c:\dir\a.txt")),
            path_key(Path::new(r"c:\dir\A.TXT"))
        );
    }
}

//! `file:` URIs. A server echoes these back, so one file must have one spelling:
//! every URI we send comes from a `CanonPath`.

use std::path::PathBuf;
use std::str::FromStr;

use lsp_types::Uri;
use platform::CanonPath;

pub fn file_uri(path: &CanonPath) -> Option<Uri> {
    Uri::from_str(&path.to_uri()).ok()
}

fn hex(byte: u8) -> Option<u8> {
    (byte as char).to_digit(16).map(|digit| digit as u8)
}

fn decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'%' {
            let high = hex(*bytes.get(at + 1)?)?;
            let low = hex(*bytes.get(at + 2)?)?;
            out.push(high << 4 | low);
            at += 3;
        } else {
            out.push(bytes[at]);
            at += 1;
        }
    }
    String::from_utf8(out).ok()
}

fn is_drive(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() == 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

/// The path of a URI from a server, `None` for any scheme but `file:`.
pub fn file_path(uri: &Uri) -> Option<PathBuf> {
    let rest = uri.as_str().strip_prefix("file://")?;
    let rest = decode(rest)?;
    let path = rest.strip_prefix('/').map_or_else(
        || format!(r"\\{}", rest.replace('/', "\\")),
        |local| {
            let (drive, tail) = local.split_at_checked(2).unwrap_or((local, ""));
            if is_drive(drive) {
                format!("{}{}", drive.to_ascii_uppercase(), tail.replace('/', "\\"))
            } else {
                format!(r"\{}", local.replace('/', "\\"))
            }
        },
    );
    Some(PathBuf::from(path))
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

    #[test]
    fn a_canonical_path_becomes_a_valid_uri() {
        assert_eq!(uri(r"z:\missing\.\a b.txt"), "file:///Z:/missing/a%20b.txt");
    }

    #[test]
    fn drive_letters_are_upper_case_and_percent_decoded() {
        assert_eq!(path("file:///c:/src/A.txt"), Path::new(r"C:\src\A.txt"));
        assert_eq!(path("file:///c%3A/src/A.txt"), Path::new(r"C:\src\A.txt"));
    }

    #[test]
    fn a_path_survives_the_way_there_and_back() {
        let text = r"Z:\Проекты\a b\c#d+e.txt";
        assert_eq!(path(&uri(text)), Path::new(text));
    }

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

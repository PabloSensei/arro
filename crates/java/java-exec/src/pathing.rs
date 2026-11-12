use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

use base::Result;
use project_model::cache_file;
use zip::CompressionMethod;
use zip::write::{SimpleFileOptions, ZipWriter};

/// The limit of a manifest line in bytes, a continuation line counts its leading space.
const LINE_LIMIT: usize = 72;

/// Stored under its content: a running program keeps its jar locked, so the same classpath
/// reuses the jar and a changed one never has to touch it.
pub fn classpath_jar(cache_root: &Path, project: &Path, paths: &[PathBuf]) -> Result<PathBuf> {
    let manifest = manifest(paths);
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in manifest.bytes() {
        hash = (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3);
    }
    let directory = cache_file(cache_root, project).with_file_name("run");
    let file = directory.join(format!("classpath-{hash:016x}.jar"));
    if !file.is_file() {
        fs::create_dir_all(&directory)?;
        write_jar(&file, &manifest)?;
    }
    Ok(file)
}

/// A jar with nothing inside but a manifest that lists the classpath. The command line
/// would overflow on a large project, and `@argfile` does not work on JDK 8.
pub fn write_classpath_jar(file: &Path, paths: &[PathBuf]) -> Result {
    write_jar(file, &manifest(paths))
}

fn write_jar(file: &Path, manifest: &str) -> Result {
    let temporary = file.with_extension("tmp");
    let mut jar = ZipWriter::new(File::create(&temporary)?);
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    jar.start_file("META-INF/MANIFEST.MF", options)?;
    jar.write_all(manifest.as_bytes())?;
    jar.finish()?;
    fs::rename(temporary, file)?;
    Ok(())
}

fn manifest(paths: &[PathBuf]) -> String {
    let urls: Vec<String> = paths.iter().map(|path| url_of(path)).collect();
    let attribute = format!("Class-Path: {}", urls.join(" "));
    let mut text = "Manifest-Version: 1.0\r\n".to_owned();
    // All ASCII after percent-encoding, so a line can break between any two bytes.
    let mut rest = attribute.as_str();
    let mut limit = LINE_LIMIT;
    while rest.len() > limit {
        let (line, tail) = rest.split_at(limit);
        text.push_str(line);
        text.push_str("\r\n ");
        rest = tail;
        limit = LINE_LIMIT - 1;
    }
    text.push_str(rest);
    text.push_str("\r\n\r\n");
    text
}

/// A directory needs the trailing slash, otherwise the JVM takes it for a jar. Output
/// directories may not exist yet, so anything that is not a file counts as a directory.
fn url_of(path: &Path) -> String {
    let text = path.to_string_lossy();
    let text = text
        .strip_prefix(r"\\?\")
        .unwrap_or(&text)
        .replace('\\', "/");
    let mut url = match text.strip_prefix("//") {
        Some(share) => format!("file://{}", encode(share)),
        None => format!("file:/{}", encode(&text)),
    };
    if !path.is_file() && !url.ends_with('/') {
        url.push('/');
    }
    url
}

fn encode(path: &str) -> String {
    let mut encoded = String::with_capacity(path.len());
    for (index, byte) in path.bytes().enumerate() {
        let is_drive_colon = index == 1 && byte == b':';
        if is_drive_colon || byte.is_ascii_alphanumeric() || b"/-._~".contains(&byte) {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        test_support::scratch(&format!("exec-{name}"))
    }

    /// What a manifest reader sees: continuation lines glued back together.
    fn class_path(manifest: &str) -> String {
        let joined = manifest.replace("\r\n ", "");
        let line = joined.lines().find(|line| line.starts_with("Class-Path: "));
        line.unwrap()["Class-Path: ".len()..].to_owned()
    }

    #[test]
    fn spaces_and_cyrillic_are_percent_encoded() {
        let url = url_of(Path::new(r"C:\Program Files\проект\classes"));

        assert_eq!(
            url,
            "file:/C:/Program%20Files/%D0%BF%D1%80%D0%BE%D0%B5%D0%BA%D1%82/classes/"
        );
    }

    #[test]
    fn a_share_keeps_its_host() {
        assert_eq!(
            url_of(Path::new(r"\\server\share\out")),
            "file://server/share/out/"
        );
    }

    #[test]
    fn a_verbatim_prefix_is_dropped() {
        assert_eq!(url_of(Path::new(r"\\?\C:\out")), "file:/C:/out/");
    }

    #[test]
    fn a_file_has_no_trailing_slash() {
        let directory = temp_dir("file");
        let file = directory.join("a.jar");
        fs::write(&file, "").unwrap();

        assert!(url_of(&directory).ends_with('/'));
        assert!(url_of(&file).ends_with("a.jar"));
    }

    #[test]
    fn long_paths_are_split_into_short_lines_without_loss() {
        let paths: Vec<PathBuf> = (0..50)
            .map(|index| format!("C:/libraries/group/artifact-{index}/version/some.jar").into())
            .collect();

        let text = manifest(&paths);

        for line in text.split("\r\n") {
            assert!(line.len() <= LINE_LIMIT, "{line}");
        }
        assert_eq!(class_path(&text).split(' ').count(), 50);
        assert!(text.ends_with("\r\n\r\n"));
    }

    #[test]
    fn a_short_classpath_stays_on_one_line() {
        let text = manifest(&[PathBuf::from("C:/a")]);

        assert_eq!(
            text,
            "Manifest-Version: 1.0\r\nClass-Path: file:/C:/a/\r\n\r\n"
        );
    }

    #[test]
    fn the_same_classpath_reuses_the_jar_and_another_gets_its_own() {
        let cache = temp_dir("cache");
        let project = Path::new("project");
        let first = classpath_jar(&cache, project, &[PathBuf::from("C:/a")]).unwrap();
        let written = fs::metadata(&first).unwrap().modified().unwrap();

        let again = classpath_jar(&cache, project, &[PathBuf::from("C:/a")]).unwrap();
        let other = classpath_jar(&cache, project, &[PathBuf::from("C:/b")]).unwrap();

        assert_eq!(first, again);
        assert_eq!(fs::metadata(&again).unwrap().modified().unwrap(), written);
        assert_ne!(first, other);
    }
}

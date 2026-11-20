//! The archive of the window settings: a zip with a manifest and the files of the
//! settings folder. The manifest names every part with a plain file name, and an import
//! writes only the parts the manifest names, so no entry can leave the folder.

use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

/// The name of the manifest inside the archive.
pub const MANIFEST_FILE: &str = "manifest.json";
/// The format the manifest names, so that another zip is refused.
pub const ARCHIVE_FORMAT: &str = "arro-settings";
/// The version of the archive this build writes and reads.
pub const ARCHIVE_VERSION: u32 = 1;
/// The largest part an import accepts, in bytes.
const MAX_PART_BYTES: u64 = 32 * 1024 * 1024;
/// The largest manifest an import reads, in bytes.
const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;

/// One part of an archive.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Part {
    /// The name the import dialog switches on and off.
    pub id: String,
    /// The line the dialog shows.
    pub title: String,
    /// The plain file name inside the archive and in the settings folder.
    pub file: String,
}

/// What the archive holds.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub format: String,
    pub version: u32,
    pub parts: Vec<Part>,
}

/// One file an export writes.
#[derive(Clone, Debug)]
pub struct ExportPart<'a> {
    pub id: &'a str,
    pub title: &'a str,
    pub file: &'a str,
    pub text: String,
}

/// Writes the parts and their manifest into a new archive. Existing files are replaced.
pub fn export(destination: &Path, parts: &[ExportPart<'_>]) -> Result<Manifest, String> {
    let mut manifest = Manifest {
        format: ARCHIVE_FORMAT.to_owned(),
        version: ARCHIVE_VERSION,
        parts: Vec::new(),
    };
    for part in parts {
        let file = plain_name(part.file)
            .ok_or_else(|| format!("{} is not a plain file name", part.file))?;
        if part.id.is_empty() {
            return Err("a part of the settings has no name".to_owned());
        }
        if manifest.parts.iter().any(|known| known.file == file) {
            return Err(format!("{file} is written twice"));
        }
        manifest.parts.push(Part {
            id: part.id.to_owned(),
            title: part.title.to_owned(),
            file: file.to_owned(),
        });
    }
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let file = std::fs::File::create(destination).map_err(|error| error.to_string())?;
    let mut archive = ZipWriter::new(file);
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .unix_permissions(0o644);
    let manifest_text =
        serde_json::to_string_pretty(&manifest).map_err(|error| error.to_string())?;
    archive
        .start_file(MANIFEST_FILE, options)
        .map_err(|error| error.to_string())?;
    archive
        .write_all(manifest_text.as_bytes())
        .map_err(|error| error.to_string())?;
    for part in parts {
        archive
            .start_file(part.file, options)
            .map_err(|error| error.to_string())?;
        archive
            .write_all(part.text.as_bytes())
            .map_err(|error| error.to_string())?;
    }
    archive.finish().map_err(|error| error.to_string())?;
    Ok(manifest)
}

/// The manifest of an archive, without reading its parts.
pub fn read_manifest(archive: &Path) -> Result<Manifest, String> {
    let file =
        std::fs::File::open(archive).map_err(|error| format!("{}: {error}", archive.display()))?;
    let mut files = ZipArchive::new(file)
        .map_err(|_| format!("{} is not an Arro settings archive", archive.display()))?;
    let manifest = {
        let mut entry = files
            .by_name(MANIFEST_FILE)
            .map_err(|_| format!("{} is not an Arro settings archive", archive.display()))?;
        if entry.size() > MAX_MANIFEST_BYTES {
            return Err(format!(
                "{} has a manifest that is too large",
                archive.display()
            ));
        }
        let mut text = String::new();
        entry
            .by_ref()
            .take(MAX_MANIFEST_BYTES + 1)
            .read_to_string(&mut text)
            .map_err(|error| format!("{}: {error}", archive.display()))?;
        if text.len() as u64 > MAX_MANIFEST_BYTES {
            return Err(format!(
                "{} has a manifest that is too large",
                archive.display()
            ));
        }
        text
    };
    check_manifest(
        serde_json::from_str::<Manifest>(&manifest)
            .map_err(|_| format!("{} has a broken manifest", archive.display()))?,
        archive,
    )
}

/// The text of a part named by a manifest, for the preview of an import.
pub fn read_part(archive: &Path, part: &Part) -> Result<String, String> {
    let file =
        std::fs::File::open(archive).map_err(|error| format!("{}: {error}", archive.display()))?;
    let mut files = ZipArchive::new(file)
        .map_err(|_| format!("{} is not an Arro settings archive", archive.display()))?;
    let name =
        plain_name(&part.file).ok_or_else(|| format!("{} is not a plain file name", part.file))?;
    let mut entry = files
        .by_name(name)
        .map_err(|_| format!("{} has no {name}", archive.display()))?;
    let mut text = String::new();
    entry
        .by_ref()
        .take(MAX_PART_BYTES + 1)
        .read_to_string(&mut text)
        .map_err(|error| format!("{name}: {error}"))?;
    if text.len() as u64 > MAX_PART_BYTES {
        return Err(format!("{name} is too large"));
    }
    Ok(text)
}

/// The text entries of a zip whose names `wanted` accepts, as `(name, text)`. Only those
/// entries are opened: an entry of another kind is never read.
pub fn read_wanted_texts(
    archive: &Path,
    wanted: impl Fn(&str) -> bool,
) -> Result<Vec<(String, String)>, String> {
    let file =
        std::fs::File::open(archive).map_err(|error| format!("{}: {error}", archive.display()))?;
    let mut files =
        ZipArchive::new(file).map_err(|_| format!("{} is not an archive", archive.display()))?;
    let names: Vec<String> = files
        .file_names()
        .filter_map(Result::ok)
        .filter(|name| wanted(name))
        .map(|name| name.into_owned())
        .collect();
    let mut texts = Vec::with_capacity(names.len());
    for name in names {
        let mut entry = files
            .by_name(&name)
            .map_err(|_| format!("{} has no {name}", archive.display()))?;
        if entry.is_dir() {
            continue;
        }
        if entry.size() > MAX_PART_BYTES {
            return Err(format!("{name} is too large"));
        }
        let mut text = String::new();
        entry
            .by_ref()
            .take(MAX_PART_BYTES + 1)
            .read_to_string(&mut text)
            .map_err(|error| format!("{name}: {error}"))?;
        if text.len() as u64 > MAX_PART_BYTES {
            return Err(format!("{name} is too large"));
        }
        texts.push((name, text));
    }
    Ok(texts)
}

/// Writes the selected parts of an archive into `settings_dir` and gives their ids. A
/// part the user did not select is left alone, as is every file the manifest does not
/// name.
pub fn import(
    archive_path: &Path,
    settings_dir: &Path,
    selected: &[String],
) -> Result<Vec<String>, String> {
    let manifest = read_manifest(archive_path)?;
    let file = std::fs::File::open(archive_path)
        .map_err(|error| format!("{}: {error}", archive_path.display()))?;
    let mut files = ZipArchive::new(file)
        .map_err(|_| format!("{} is not an Arro settings archive", archive_path.display()))?;
    let mut written = Vec::new();
    for part in &manifest.parts {
        if !selected.iter().any(|id| id == &part.id) {
            continue;
        }
        let name = plain_name(&part.file)
            .ok_or_else(|| format!("{} is not a plain file name", part.file))?;
        let mut entry = files
            .by_name(name)
            .map_err(|_| format!("{} has no {name}", archive_path.display()))?;
        if entry.size() > MAX_PART_BYTES {
            return Err(format!("{name} is too large"));
        }
        let mut bytes = Vec::with_capacity(entry.size() as usize);
        entry
            .by_ref()
            .take(MAX_PART_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| format!("{name}: {error}"))?;
        if bytes.len() as u64 > MAX_PART_BYTES {
            return Err(format!("{name} is too large"));
        }
        write_beside(settings_dir, name, &bytes)?;
        written.push(part.id.clone());
    }
    Ok(written)
}

/// Writes one file of the settings folder through a temporary file, so a stop in the
/// middle leaves the earlier file whole.
fn write_beside(directory: &Path, name: &str, bytes: &[u8]) -> Result<(), String> {
    std::fs::create_dir_all(directory).map_err(|error| error.to_string())?;
    let target = directory.join(name);
    let temporary = temporary_path(&target);
    std::fs::write(&temporary, bytes).map_err(|error| error.to_string())?;
    if let Err(error) = std::fs::rename(&temporary, &target) {
        let _ = std::fs::remove_file(&temporary);
        return Err(error.to_string());
    }
    Ok(())
}

fn temporary_path(path: &Path) -> PathBuf {
    let mut name = path
        .file_name()
        .map_or_else(|| "settings".into(), |name| name.to_os_string());
    name.push(format!(".{}.tmp", std::process::id()));
    path.with_file_name(name)
}

/// A file name the import may write: one plain component, not a folder and not a way up.
fn plain_name(name: &str) -> Option<&str> {
    if name.is_empty() || name.contains(['/', '\\', ':']) || name == "." || name == ".." {
        return None;
    }
    let mut components = Path::new(name).components();
    match (components.next(), components.next()) {
        (Some(Component::Normal(part)), None) if part == name => Some(name),
        _ => None,
    }
}

fn check_manifest(manifest: Manifest, archive: &Path) -> Result<Manifest, String> {
    if manifest.format != ARCHIVE_FORMAT {
        return Err(format!(
            "{} is not an Arro settings archive",
            archive.display()
        ));
    }
    if manifest.version > ARCHIVE_VERSION {
        return Err(format!(
            "{} was written by a newer Arro: version {}",
            archive.display(),
            manifest.version
        ));
    }
    for part in &manifest.parts {
        if part.id.is_empty() || plain_name(&part.file).is_none() {
            return Err(format!("{} has a broken manifest", archive.display()));
        }
        if manifest
            .parts
            .iter()
            .filter(|other| other.id == part.id)
            .count()
            > 1
        {
            return Err(format!("{} names a part twice", archive.display()));
        }
    }
    Ok(manifest)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{
        ARCHIVE_FORMAT, ExportPart, MANIFEST_FILE, Manifest, export, import, read_manifest,
    };

    fn scratch(name: &str) -> std::path::PathBuf {
        test_support::scratch(&format!("settings-archive-{name}"))
    }

    fn parts() -> Vec<ExportPart<'static>> {
        vec![
            ExportPart {
                id: "settings",
                title: "Settings",
                file: "settings.json",
                text: "{\"font_size\": 15}".to_owned(),
            },
            ExportPart {
                id: "keymap",
                title: "Keymap",
                file: "keymap.json",
                text: "{\"bindings\": []}".to_owned(),
            },
        ]
    }

    #[test]
    fn a_round_trip_restores_the_parts_the_import_selects() {
        let root = scratch("round-trip");
        let archive = root.join("arro-settings.zip");
        let manifest = export(&archive, &parts()).unwrap();
        assert_eq!(manifest.format, ARCHIVE_FORMAT);
        assert_eq!(manifest.parts.len(), 2);

        let settings = root.join("other");
        let written = import(&archive, &settings, &["settings".to_owned()]).unwrap();
        assert_eq!(written, ["settings"]);
        assert_eq!(
            fs::read_to_string(settings.join("settings.json")).unwrap(),
            "{\"font_size\": 15}"
        );
        assert!(
            !settings.join("keymap.json").exists(),
            "only the choice is written"
        );
    }

    #[test]
    fn a_file_that_is_not_an_archive_is_refused() {
        let root = scratch("broken");
        let archive = root.join("broken.zip");
        fs::write(&archive, b"not a zip").unwrap();
        assert!(read_manifest(&archive).is_err());
        assert!(import(&archive, &root.join("settings"), &[]).is_err());
    }

    #[test]
    fn a_part_outside_the_folder_is_refused() {
        let root = scratch("slip");
        let archive = root.join("slip.zip");
        let file = fs::File::create(&archive).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default();
        let manifest = Manifest {
            format: ARCHIVE_FORMAT.to_owned(),
            version: 1,
            parts: vec![super::Part {
                id: "settings".to_owned(),
                title: "Settings".to_owned(),
                file: "../outside.json".to_owned(),
            }],
        };
        zip.start_file(MANIFEST_FILE, options).unwrap();
        std::io::Write::write_all(
            &mut zip,
            serde_json::to_string(&manifest).unwrap().as_bytes(),
        )
        .unwrap();
        zip.start_file("../outside.json", options).unwrap();
        std::io::Write::write_all(&mut zip, b"{\"font_size\": 99}").unwrap();
        zip.finish().unwrap();

        let settings = root.join("settings");
        assert!(import(&archive, &settings, &["settings".to_owned()]).is_err());
        assert!(!root.join("outside.json").exists());
        assert!(!settings.join("outside.json").exists());
    }
}

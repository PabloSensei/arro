//! New projects: the fields a form asks, the values it collects and what a generator
//! writes into the folder. A generator is a tool behind an interface, so this crate
//! knows neither the language nor the build tool of a project.

use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use base::Cancel;

/// One value of a choice field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Choice {
    pub value: String,
    pub label: String,
    /// The group of a long list, such as the dependencies of a generator.
    pub group: Option<String>,
    /// What the value means, shown beside it in a long list.
    pub hint: Option<String>,
}

impl Choice {
    pub fn new(value: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            label: label.into(),
            group: None,
            hint: None,
        }
    }

    pub fn in_group(mut self, group: impl Into<String>) -> Self {
        self.group = Some(group.into());
        self
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }
}

/// How the form shows a field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    Text,
    /// One value out of a short list.
    Choice,
    /// Many values out of a long, grouped and searchable list.
    Many,
    Toggle,
}

/// One field under the common ones of the form.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Field {
    pub key: String,
    pub label: String,
    pub kind: Kind,
    /// The value of a text field or the chosen one of a choice, empty when none.
    pub default: String,
    pub choices: Vec<Choice>,
    pub hint: Option<String>,
    /// Shown only while another field has this value, as the script dialect of a build.
    pub shown_when: Option<(String, String)>,
    /// Whether the field starts with the name of the project in the form of an artifact.
    pub from_name: bool,
}

impl Field {
    pub fn text(key: &str, label: &str, default: &str) -> Self {
        Self::new(key, label, Kind::Text, default)
    }

    pub fn choice(key: &str, label: &str, choices: Vec<Choice>) -> Self {
        let default = choices.first().map(|choice| choice.value.clone());
        let mut field = Self::new(key, label, Kind::Choice, default.as_deref().unwrap_or(""));
        field.choices = choices;
        field
    }

    pub fn many(key: &str, label: &str, choices: Vec<Choice>) -> Self {
        let mut field = Self::new(key, label, Kind::Many, "");
        field.choices = choices;
        field
    }

    pub fn toggle(key: &str, label: &str, on: bool) -> Self {
        Self::new(key, label, Kind::Toggle, if on { "true" } else { "false" })
    }

    fn new(key: &str, label: &str, kind: Kind, default: &str) -> Self {
        Self {
            key: key.to_owned(),
            label: label.to_owned(),
            kind,
            default: default.to_owned(),
            choices: Vec::new(),
            hint: None,
            shown_when: None,
            from_name: false,
        }
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    /// The field follows the project name until the user types into it.
    pub fn from_name(mut self) -> Self {
        self.from_name = true;
        self
    }

    pub fn shown_when(mut self, key: &str, value: &str) -> Self {
        self.shown_when = Some((key.to_owned(), value.to_owned()));
        self
    }

    /// The default choice among the values, when the field has one.
    pub fn set_default(&mut self, value: &str) {
        if self.choices.iter().any(|choice| choice.value == value) {
            self.default = value.to_owned();
        }
    }
}

/// What the user typed into the fields, by the key of the field.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Values {
    text: BTreeMap<String, String>,
    many: BTreeMap<String, Vec<String>>,
    flags: BTreeMap<String, bool>,
}

impl Values {
    pub fn set(&mut self, key: &str, value: impl Into<String>) {
        self.text.insert(key.to_owned(), value.into());
    }

    pub fn set_many(&mut self, key: &str, values: Vec<String>) {
        self.many.insert(key.to_owned(), values);
    }

    pub fn set_flag(&mut self, key: &str, on: bool) {
        self.flags.insert(key.to_owned(), on);
    }

    pub fn text(&self, key: &str) -> &str {
        self.text.get(key).map(String::as_str).unwrap_or_default()
    }

    pub fn many(&self, key: &str) -> &[String] {
        self.many.get(key).map(Vec::as_slice).unwrap_or_default()
    }

    pub fn flag(&self, key: &str) -> bool {
        self.flags.get(key).copied().unwrap_or(false)
    }

    /// The value as a number, `fallback` when the field is empty or not a number.
    pub fn feature(&self, key: &str, fallback: u32) -> u32 {
        self.text(key).trim().parse().unwrap_or(fallback)
    }
}

/// What a generator needs to write a project.
#[derive(Clone, Debug)]
pub struct Request {
    /// The name the user gave the project.
    pub name: String,
    /// The folder of the project, absolute and not written yet.
    pub folder: PathBuf,
    pub values: Values,
    /// Whether the folder becomes a repository with a `.gitignore`.
    pub git: bool,
}

/// What a generator gives back for the form to write into the folder.
#[derive(Clone, Debug, Default)]
pub struct Created {
    /// The files of the project, as paths inside the folder and their text.
    pub files: Vec<(String, String)>,
    /// An archive of the project, unpacked into the folder.
    pub archive: Option<Vec<u8>>,
    /// What the user has to know about the project that was written.
    pub note: Option<String>,
}

impl Created {
    pub fn files(files: Vec<(String, String)>) -> Self {
        Self {
            files,
            ..Self::default()
        }
    }

    pub fn archive(bytes: Vec<u8>) -> Self {
        Self {
            archive: Some(bytes),
            ..Self::default()
        }
    }

    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.note = Some(note.into());
        self
    }
}

/// A tool that makes a new project. The application supplies the generators; the form
/// knows only their fields and their label.
pub trait Generator: Send + Sync {
    /// The name of the generator in the list of the form.
    fn label(&self) -> &str;

    /// The fields the form shows at once. Cheap: no network and no long wait.
    fn fields(&self) -> Vec<Field>;

    /// Whether the choices come from outside, so the form offers to ask again.
    fn loads(&self) -> bool {
        false
    }

    /// The same fields with the choices that come from outside, such as a server.
    /// Blocks, so the form calls it away from its thread. The fields the answer does not
    /// name stay as they were.
    fn load(&self, _values: &Values, _cancel: &Cancel) -> Result<Vec<Field>, String> {
        Ok(self.fields())
    }

    /// The files or the archive of the project.
    fn create(&self, request: &Request, cancel: &Cancel) -> Result<Created, String>;

    /// The step that needs the files on disk, such as a wrapper of the build tool.
    /// Answers the note the form shows once the project is written.
    fn finish(
        &self,
        _request: &Request,
        _folder: &Path,
        _cancel: &Cancel,
    ) -> Result<Option<String>, String> {
        Ok(None)
    }
}

/// The largest project an archive may unpack to.
const MAX_UNPACKED_BYTES: u64 = 64 * 1024 * 1024;
/// The most entries an archive of a project may have.
const MAX_ENTRIES: usize = 10_000;

/// Makes the folder of a new project: a missing folder is created, a folder with files
/// in it is refused, so a new project never writes over the work of the user.
pub fn prepare(folder: &Path) -> Result<(), String> {
    if folder.exists() {
        let mut entries = fs::read_dir(folder)
            .map_err(|error| format!("{} cannot be read: {error}", folder.display()))?;
        if entries.next().is_some() {
            return Err(format!("The folder {} is not empty", folder.display()));
        }
        return Ok(());
    }
    fs::create_dir_all(folder)
        .map_err(|error| format!("{} cannot be created: {error}", folder.display()))
}

/// Why the folder cannot take a new project, if it cannot.
pub fn folder_problem(folder: &Path) -> Option<String> {
    if !folder.exists() {
        return None;
    }
    let mut entries = fs::read_dir(folder).ok()?;
    entries
        .next()
        .is_some()
        .then(|| format!("The folder {} is not empty", folder.display()))
}

/// Writes what a generator made into the folder the form prepared.
pub fn apply_created(folder: &Path, created: &Created) -> Result<(), String> {
    for (relative, text) in &created.files {
        write_inside(folder, relative, text.as_bytes())?;
    }
    if let Some(bytes) = &created.archive {
        unpack_zip(bytes, folder)?;
    }
    Ok(())
}

/// Writes one file, refusing a path that leaves `folder`.
fn write_inside(folder: &Path, relative: &str, bytes: &[u8]) -> Result<(), String> {
    let inside = inside_path(relative)
        .ok_or_else(|| format!("The path {relative} leaves the project folder"))?;
    let path = folder.join(inside);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("{} cannot be created: {error}", parent.display()))?;
    }
    fs::write(&path, bytes)
        .map_err(|error| format!("{} cannot be written: {error}", path.display()))
}

/// A project name as the artifact of a build file: lower case words joined by dashes.
pub fn artifact_name(name: &str) -> String {
    let dashed: String = name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let dashed = dashed.trim_matches('-').to_owned();
    if dashed.is_empty() {
        "demo".to_owned()
    } else {
        dashed
    }
}

/// A relative path of an archive entry, `None` for an absolute one or one with `..`.
fn inside_path(relative: &str) -> Option<PathBuf> {
    let unified = relative.replace('\\', "/");
    if unified.starts_with('/') || unified.contains(':') {
        return None;
    }
    let mut path = PathBuf::new();
    for part in unified.split('/') {
        match part {
            "" | "." => {}
            ".." => return None,
            other => path.push(other),
        }
    }
    let leaves = path
        .components()
        .all(|component| matches!(component, Component::Normal(_)));
    (leaves && !path.as_os_str().is_empty()).then_some(path)
}

/// Unpacks a zip archive into the folder, refusing every entry that would land outside
/// it. An archive of one project keeps its files under one root folder, which is
/// stripped, so the files land where the folder is.
pub fn unpack_zip(bytes: &[u8], folder: &Path) -> Result<(), String> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))
        .map_err(|error| format!("The archive cannot be read: {error}"))?;
    if archive.len() > MAX_ENTRIES {
        return Err("The archive has too many entries".to_owned());
    }
    let names: Vec<String> = (0..archive.len())
        .map(|index| {
            archive
                .by_index(index)
                .map_err(|error| format!("The archive cannot be read: {error}"))?
                .name()
                .map(|name| name.into_owned())
                .map_err(|error| format!("The archive cannot be read: {error}"))
        })
        .collect::<Result<_, _>>()?;
    let root = common_root(&names);
    let mut unpacked = 0u64;
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|error| format!("The archive cannot be read: {error}"))?;
        let name = entry
            .name()
            .map(|name| name.into_owned())
            .map_err(|error| format!("The archive cannot be read: {error}"))?;
        let relative = match &root {
            Some(root) => name
                .strip_prefix(root.as_str())
                .map(|rest| rest.trim_start_matches(['/', '\\'])),
            None => Some(name.as_str()),
        };
        let Some(relative) = relative else {
            continue;
        };
        let Some(path) = inside_path(relative) else {
            return Err(format!("The entry {name} leaves the project folder"));
        };
        if entry.is_dir() {
            fs::create_dir_all(folder.join(path))
                .map_err(|error| format!("{} cannot be created: {error}", folder.display()))?;
            continue;
        }
        unpacked = unpacked.saturating_add(entry.size());
        if unpacked > MAX_UNPACKED_BYTES {
            return Err("The archive is larger than a project may be".to_owned());
        }
        let mut text = Vec::new();
        entry
            .read_to_end(&mut text)
            .map_err(|error| format!("The archive cannot be read: {error}"))?;
        let inside = path.to_string_lossy().replace('\\', "/");
        write_inside(folder, &inside, &text)?;
    }
    Ok(())
}

/// The one folder every entry of an archive stands in, with its slash, as the archive of
/// a generator names it. `None` when the entries share no such folder.
fn common_root(names: &[String]) -> Option<String> {
    let mut root: Option<String> = None;
    for name in names {
        let unified = name.replace('\\', "/");
        let (first, rest) = unified.split_once('/')?;
        if first.is_empty() || rest.is_empty() || first.contains(':') {
            return None;
        }
        match &root {
            Some(known) if known != first => return None,
            Some(_) => {}
            None => root = Some(first.to_owned()),
        }
    }
    root.map(|root| format!("{root}/"))
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    fn zip_of(files: &[(&str, &str)]) -> Vec<u8> {
        let mut bytes = Vec::new();
        {
            let mut writer = zip::ZipWriter::new(std::io::Cursor::new(&mut bytes));
            for (name, text) in files {
                let options = zip::write::SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Stored);
                writer.start_file(*name, options).unwrap();
                writer.write_all(text.as_bytes()).unwrap();
            }
            writer.finish().unwrap();
        }
        bytes
    }

    #[test]
    fn an_archive_lands_under_the_folder_of_the_project() {
        let root = test_support::scratch("scaffold-unpack");
        let folder = root.join("demo");
        prepare(&folder).unwrap();
        let bytes = zip_of(&[
            ("demo/pom.xml", "<project/>"),
            ("demo/src/main/App.txt", "class App {}"),
        ]);

        apply_created(&folder, &Created::archive(bytes)).unwrap();

        assert_eq!(
            fs::read_to_string(folder.join("pom.xml")).unwrap(),
            "<project/>"
        );
        assert!(folder.join("src/main/App.txt").is_file());
        assert!(!folder.join("demo").exists(), "the root folder is stripped");
    }

    #[test]
    fn an_entry_that_leaves_the_folder_is_refused() {
        let root = test_support::scratch("scaffold-slip");
        let folder = root.join("demo");
        prepare(&folder).unwrap();
        let bytes = zip_of(&[
            ("demo/pom.xml", "<project/>"),
            ("demo/../../evil.txt", "no"),
        ]);

        let error = apply_created(&folder, &Created::archive(bytes)).unwrap_err();

        assert!(error.contains("leaves the project folder"), "{error}");
        assert!(!root.join("evil.txt").exists());
        assert!(!root.parent().unwrap().join("evil.txt").exists());
    }

    #[test]
    fn an_absolute_entry_is_refused() {
        let root = test_support::scratch("scaffold-absolute");
        let folder = root.join("demo");
        prepare(&folder).unwrap();
        let bytes = zip_of(&[("C:/Windows/evil.txt", "no")]);

        assert!(apply_created(&folder, &Created::archive(bytes)).is_err());
    }

    #[test]
    fn a_folder_with_files_is_never_written_over() {
        let root = test_support::scratch("scaffold-busy");
        let folder = root.join("demo");
        fs::create_dir_all(&folder).unwrap();
        fs::write(folder.join("keep.txt"), "mine").unwrap();

        let error = prepare(&folder).unwrap_err();

        assert!(error.contains("not empty"), "{error}");
        assert!(folder_problem(&folder).is_some());
        assert_eq!(fs::read_to_string(folder.join("keep.txt")).unwrap(), "mine");
    }

    #[test]
    fn a_missing_folder_is_created_and_an_archive_without_one_root_keeps_its_paths() {
        let root = test_support::scratch("scaffold-flat");
        let folder = root.join("demo");
        let bytes = zip_of(&[("pom.xml", "<project/>"), ("README.md", "hello")]);

        apply_created(&folder, &Created::archive(bytes)).unwrap();

        assert!(folder.join("pom.xml").is_file());
        assert_eq!(
            fs::read_to_string(folder.join("README.md")).unwrap(),
            "hello"
        );
    }

    #[test]
    fn files_with_a_path_that_leaves_the_folder_are_refused() {
        let root = test_support::scratch("scaffold-files");
        let folder = root.join("demo");
        prepare(&folder).unwrap();
        let created = Created::files(vec![("../evil.txt".to_owned(), "no".to_owned())]);

        assert!(apply_created(&folder, &created).is_err());
        assert!(!root.join("evil.txt").exists());
    }

    #[test]
    fn a_field_starts_with_the_first_choice_or_its_value() {
        let field = Field::choice(
            "tool",
            "Tool",
            vec![
                Choice::new("first", "First"),
                Choice::new("second", "Second"),
            ],
        );
        assert_eq!(field.default, "first");

        let mut field = field;
        field.set_default("second");
        assert_eq!(field.default, "second");
        field.set_default("missing");
        assert_eq!(field.default, "second", "an unknown value is not chosen");
    }
}

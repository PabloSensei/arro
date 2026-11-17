//! The sets of icons the window offers: the built-in `arro` and `intellij`, and the
//! folders of the settings. Every set names its icons by the logical name the window
//! asks for, in a light file and a file with the `_dark` suffix.
//!
//! `arro` is the icon folder of this crate and comes with every icon. `intellij` keeps
//! the files of the IntelliJ New UI set under the paths of its source and finds them
//! through `map.toml`. A set of the user is a folder of `icons` in the settings folder.
//! An icon a set does not have comes from `arro`, so the window never shows a gap.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{OnceLock, RwLock};

use gpui_kit::*;

use crate::embedded::{embedded_asset, register_asset};

/// The icons of the crate: the default set.
pub const ARRO: &str = "arro";
/// The icons of the IntelliJ New UI.
pub const INTELLIJ: &str = "intellij";

/// The table of the `intellij` set, compiled in with the icons it names.
const INTELLIJ_MAP: &str = include_str!("../assets/icon-sets/intellij/map.toml");

/// One set as the Settings window lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IconSet {
    /// The name the settings save.
    pub id: String,
    /// The name the list shows.
    pub name: String,
}

/// The sets a window offers: the built-in ones and the folders of the settings.
#[derive(Clone, Debug, Default)]
pub struct IconCatalog {
    sets: Vec<IconSet>,
    folder: PathBuf,
}

impl Global for IconCatalog {}

impl IconCatalog {
    /// Reads the `icons` folder of a settings folder. Every folder with at least one SVG
    /// becomes a set; its files join the assets of the process under
    /// `icon-sets/<id>/<logical name>.svg`. Blocks on the disk: not for the UI thread.
    pub fn load(settings_dir: &Path) -> Self {
        let folder = settings_dir.join("icons");
        let mut sets = builtin_sets().to_vec();
        for (id, files) in read_sets(&folder) {
            if sets.iter().any(|set| set.id == id) {
                continue;
            }
            for (relative, data) in files {
                register_asset(format!("icon-sets/{id}/{relative}"), data);
            }
            sets.push(IconSet {
                name: id.clone(),
                id,
            });
        }
        Self { sets, folder }
    }

    /// The sets of the binary alone.
    pub fn builtin() -> Self {
        Self {
            sets: builtin_sets().to_vec(),
            folder: PathBuf::new(),
        }
    }

    /// Reads the folder again after the user added or removed a set.
    pub fn reload(&mut self) {
        if self.folder.as_os_str().is_empty() {
            return;
        }
        let settings = self.folder.parent().unwrap_or(&self.folder);
        *self = Self::load(settings);
    }

    pub fn sets(&self) -> &[IconSet] {
        &self.sets
    }

    /// The sets of the window, or the built-in ones when a window has not loaded its
    /// folder.
    pub fn sets_of(cx: &App) -> &[IconSet] {
        match cx.try_global::<IconCatalog>() {
            Some(catalog) => catalog.sets(),
            None => builtin_sets(),
        }
    }

    /// The label of a saved id, as the Settings window writes it.
    pub fn label(id: &str) -> String {
        builtin_sets()
            .iter()
            .find(|set| set.id == id)
            .map_or_else(|| id.to_owned(), |set| set.name.clone())
    }
}

fn builtin_sets() -> &'static [IconSet] {
    static SETS: OnceLock<Vec<IconSet>> = OnceLock::new();
    SETS.get_or_init(|| {
        vec![
            IconSet {
                id: ARRO.to_owned(),
                name: "Arro".to_owned(),
            },
            IconSet {
                id: INTELLIJ.to_owned(),
                name: "IntelliJ New UI".to_owned(),
            },
        ]
    })
}

/// The SVG files of one set by their logical path, and their data.
type SvgFiles = Vec<(String, Vec<u8>)>;

/// The sets of the folder of the user, sorted by id, each with its SVG files by logical
/// path. A set without an SVG is left out.
fn read_sets(folder: &Path) -> Vec<(String, SvgFiles)> {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return Vec::new();
    };
    let mut folders: Vec<PathBuf> = entries
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .map(|entry| entry.path())
        .collect();
    folders.sort();
    folders
        .into_iter()
        .filter_map(|folder| {
            let id = folder.file_name()?.to_string_lossy().into_owned();
            let files = read_svg_files(&folder, "");
            (!files.is_empty()).then_some((id, files))
        })
        .collect()
}

/// The SVG files below `folder`, keyed by their path with `/` between the folders.
fn read_svg_files(folder: &Path, prefix: &str) -> SvgFiles {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return Vec::new();
    };
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(std::fs::DirEntry::file_name);
    let mut files = Vec::new();
    for entry in entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        let relative = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        let path = entry.path();
        match entry.file_type() {
            Ok(kind) if kind.is_dir() => files.extend(read_svg_files(&path, &relative)),
            Ok(kind) if kind.is_file() && name.to_ascii_lowercase().ends_with(".svg") => {
                if let Ok(data) = std::fs::read(&path) {
                    files.push((relative, data));
                }
            }
            _ => {}
        }
    }
    files
}

/// The file of the icon `name` of `set`: the light one or the `_dark` one, at 20 px when
/// the set has that size and the caller asks for it. A name the set does not have gives
/// the file of `arro`.
pub fn icon_file(set: &str, name: &str, dark: bool, size: f32) -> SharedString {
    let fallback = arro_file(name, dark);
    if set.is_empty() || set == ARRO {
        return fallback;
    }
    let source = if set == INTELLIJ {
        intellij_source(name)
    } else {
        Some(name)
    };
    let Some(source) = source else {
        return fallback;
    };
    set_file(set, source, dark, size).map_or(fallback, SharedString::from)
}

/// The file of an icon of `arro`.
fn arro_file(name: &str, dark: bool) -> SharedString {
    // The .gitignore icon has one file for both looks.
    let suffix = if dark && name != "fileTypes/gitignore" {
        "_dark"
    } else {
        ""
    };
    format!("icons/{name}{suffix}.svg").into()
}

/// The file of an icon of a set other than `arro`, when the set has it.
fn set_file(set: &str, source: &str, dark: bool, size: f32) -> Option<String> {
    let suffix = if dark { "_dark" } else { "" };
    if size >= 20. {
        let large = format!("icon-sets/{set}/{source}@20x20{suffix}.svg");
        if asset_exists(&large) {
            return Some(large);
        }
    }
    let file = format!("icon-sets/{set}/{source}{suffix}.svg");
    asset_exists(&file).then_some(file)
}

/// The path of `name` in `map.toml`, when the table has it.
fn intellij_source(name: &str) -> Option<&'static str> {
    static MAP: OnceLock<HashMap<String, String>> = OnceLock::new();
    let map = MAP.get_or_init(|| parse_map(INTELLIJ_MAP).into_iter().collect());
    map.get(name).map(String::as_str)
}

/// The pairs of a table of the form `"logical name" = "path of the source"`. Comments
/// and the section line are left out.
pub(crate) fn parse_map(text: &str) -> Vec<(String, String)> {
    let mut pairs = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with('[') {
            continue;
        }
        let Some((name, source)) = line.split_once('=') else {
            continue;
        };
        let unquote = |text: &str| text.trim().trim_matches('"').trim().to_owned();
        let (name, source) = (unquote(name), unquote(source));
        if !name.is_empty() && !source.is_empty() {
            pairs.push((name, source));
        }
    }
    pairs
}

/// Whether an SVG of the binary or of a set of the user is at `path`.
fn asset_exists(path: &str) -> bool {
    if embedded_asset(path).is_some() {
        return true;
    }
    custom_assets()
        .read()
        .map(|assets| assets.contains_key(path))
        .unwrap_or(false)
}

fn custom_assets() -> &'static RwLock<HashMap<String, Vec<u8>>> {
    static ASSETS: OnceLock<RwLock<HashMap<String, Vec<u8>>>> = OnceLock::new();
    ASSETS.get_or_init(|| RwLock::new(HashMap::new()))
}

/// The data of an asset of a set of the user, when it has that path.
pub(crate) fn custom_asset(path: &str) -> Option<Vec<u8>> {
    custom_assets().read().ok()?.get(path).cloned()
}

/// Adds or replaces a file of a set of the user.
pub(crate) fn register_custom_asset(path: String, data: Vec<u8>) {
    if let Ok(mut assets) = custom_assets().write() {
        assets.insert(path, data);
    }
}

#[cfg(test)]
mod tests {
    use super::{ARRO, INTELLIJ, embedded_asset, icon_file, parse_map, read_sets};
    use std::collections::HashSet;

    #[test]
    fn a_set_without_a_name_takes_the_icon_of_arro() {
        assert_eq!(
            icon_file(ARRO, "general/settings", false, 16.),
            "icons/general/settings.svg"
        );
        assert_eq!(
            icon_file(ARRO, "general/settings", true, 16.),
            "icons/general/settings_dark.svg"
        );
        assert_eq!(
            icon_file(ARRO, "fileTypes/gitignore", true, 16.),
            "icons/fileTypes/gitignore.svg"
        );
        assert_eq!(
            icon_file(INTELLIJ, "fileTypes/rust", true, 16.),
            "icons/fileTypes/rust_dark.svg",
            "a name the set has no icon for comes from arro"
        );
        assert_eq!(
            icon_file(INTELLIJ, "general/settings", false, 16.),
            "icon-sets/intellij/general/settings.svg"
        );
        assert_eq!(
            icon_file(INTELLIJ, "general/settings", true, 16.),
            "icon-sets/intellij/general/settings_dark.svg"
        );
        assert_eq!(
            icon_file(INTELLIJ, "toolwindows/project", false, 20.),
            "icon-sets/intellij/toolwindows/project@20x20.svg",
            "a set with a 20 px file gives it for a large icon"
        );
        assert_eq!(
            icon_file(INTELLIJ, "nodes/folder", false, 20.),
            "icon-sets/intellij/nodes/folder.svg",
            "a set without a 20 px file keeps its 16 px one"
        );
    }

    #[test]
    fn the_map_of_the_intellij_set_parses_and_has_no_duplicates() {
        let map = parse_map(super::INTELLIJ_MAP);
        assert!(
            map.iter().any(|(name, source)| {
                name == "general/settings" && source == "general/settings"
            })
        );
        assert!(
            map.iter()
                .any(|(name, source)| { name == "general/star" && source == "nodes/star" })
        );
        let names: HashSet<&str> = map.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(
            names.len(),
            map.len(),
            "a logical name stands once in the table"
        );
        let sample = parse_map("# a comment\n[icons]\n\"a/b\" = \"c/d\"\n\n\"e\"=\"f\"");
        assert_eq!(
            sample,
            [
                ("a/b".to_owned(), "c/d".to_owned()),
                ("e".to_owned(), "f".to_owned())
            ]
        );
    }

    #[test]
    fn every_icon_of_the_map_is_in_the_binary_in_both_looks() {
        for (name, source) in parse_map(super::INTELLIJ_MAP) {
            for suffix in ["", "_dark"] {
                let path = format!("icon-sets/intellij/{source}{suffix}.svg");
                assert!(
                    embedded_asset(&path).is_some(),
                    "{name}: {path} is in the binary"
                );
            }
        }
    }

    #[test]
    fn a_folder_of_the_settings_offers_its_svgs() {
        let folder = test_support::TempDir::new("icon-sets");
        let set = folder.join("mine");
        std::fs::create_dir_all(set.join("nodes")).unwrap();
        std::fs::write(set.join("nodes/class.svg"), "<svg/>").unwrap();
        std::fs::write(set.join("nodes/class_dark.svg"), "<svg/>").unwrap();
        std::fs::write(set.join("README"), "not an icon").unwrap();
        std::fs::create_dir_all(folder.join("empty")).unwrap();
        let sets = read_sets(folder.path());
        assert_eq!(sets.len(), 1);
        assert_eq!(sets[0].0, "mine");
        let names: Vec<&str> = sets[0].1.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(names, ["nodes/class.svg", "nodes/class_dark.svg"]);
    }
}

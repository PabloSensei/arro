//! `check-icon-sets`: the bundled icon sets carry no product logos and keep the licence
//! line of their source. The `intellij` set names its files in `map.toml`; every file
//! there is in the set in both looks, and no other SVG is.
//!
//! The folders `welcome`, `ultimate`, `meetNewUi`, `survey` and `ide` hold the icons of
//! JetBrains products and their logos; the file names below are the logos of products or
//! companies. Neither may reach the binary.
//!
//! Every icon name the sources ask for (a `"group/name"` literal whose group is a folder of
//! the `arro` set) has a file in the `arro` set in both looks. A set without an icon of its
//! own shows the `arro` file, so a name without that file leaves an empty place.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use crate::Result;

/// The folders of the source that hold product logos and nothing the window uses.
const FORBIDDEN_FOLDERS: &[&str] = &["welcome", "ultimate", "meetNewUi", "survey", "ide"];

/// The names of product and company logos, lower case, without `_dark`.
const FORBIDDEN_NAMES: &[&str] = &[
    "ai",
    "idea",
    "intellij",
    "intellijidea",
    "jetbrains",
    "jetbrainsai",
    "meetnewui",
    "pycharm",
    "survey",
    "toolbox",
    "ultimate",
    "welcome",
];

/// The line every copied SVG has to keep.
const COPYRIGHT: &str = "Copyright 2000-";

/// The folder of the `arro` set, the fallback of every set.
const ARRO_FOLDER: &str = "crates/ui/ui-kit/assets/icons";
/// The icons with one file for both looks, see `arro_file` in ui-kit.
const SINGLE_LOOK: &[&str] = &["fileTypes/gitignore"];
/// Literals of the sources that have the form of an icon name but are not one: the source
/// path of an IntelliJ icon in a test of ui-kit.
const NOT_ICONS: &[&str] = &["nodes/star"];

pub fn run(root: &Path) -> Result {
    let folder = root.join("crates/ui/ui-kit/assets/icon-sets/intellij");
    let mut problems = Vec::new();
    for file in ["LICENSE", "NOTICE", "README", "map.toml"] {
        if !folder.join(file).is_file() {
            problems.push(format!("icon-sets/intellij/{file} is missing"));
        }
    }
    let map = fs::read_to_string(folder.join("map.toml"))?;
    let map = parse_map(&map);
    if map.is_empty() {
        problems.push("icon-sets/intellij/map.toml has no icons".to_owned());
    }
    let mut names = HashSet::new();
    for (name, source) in &map {
        if !names.insert(name.clone()) {
            problems.push(format!("{name} stands twice in map.toml"));
        }
        for folder in source.split('/') {
            if FORBIDDEN_FOLDERS.contains(&folder) {
                problems.push(format!("{name} copies from the forbidden folder {source}"));
            }
        }
    }
    let expected: HashSet<String> = map
        .iter()
        .flat_map(|(_, source)| [format!("{source}.svg"), format!("{source}_dark.svg")])
        .collect();
    let mut files = Vec::new();
    svgs(&folder, "", &mut files)?;
    for file in &files {
        let relative = file.strip_prefix(&folder).unwrap_or(file);
        let relative = relative.to_string_lossy().replace('\\', "/");
        let name = file
            .file_stem()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let name = name
            .strip_suffix("_dark")
            .unwrap_or(&name)
            .to_ascii_lowercase();
        if FORBIDDEN_NAMES.contains(&name.as_str()) {
            problems.push(format!("{relative} is a logo of a product"));
        }
        if !expected.contains(&relative.replace("@20x20", "")) {
            problems.push(format!("{relative} is not in map.toml"));
        }
        let text = fs::read_to_string(file)?;
        if !text.contains(COPYRIGHT) || !text.contains("Apache 2.0 license") {
            problems.push(format!("{relative} lost the copyright line of the source"));
        }
    }
    for source in expected {
        if !folder.join(&source).is_file() {
            problems.push(format!(
                "{source} is named in map.toml but is not in the set"
            ));
        }
    }
    // A name the intellij set has no file for shows the arro file, and a name it has shows
    // its own. Either way the arro file of every name has to exist.
    let arro = root.join(ARRO_FOLDER);
    let groups = icon_groups(&arro)?;
    let arro_files = arro_files(&arro)?;
    for (name, place) in icon_names(root, &groups)? {
        for dark in [false, true] {
            let file = arro_file(&name, dark);
            if !arro_files.contains(&file) {
                problems.push(format!(
                    "{name} is asked at {place}, but {ARRO_FOLDER}/{file} is missing: the icon is empty"
                ));
            }
        }
    }
    if problems.is_empty() {
        return Ok(());
    }
    problems.sort();
    problems.dedup();
    Err(problems.join("\n").into())
}

/// The SVG files below `folder`.
fn svgs(folder: &Path, prefix: &str, files: &mut Vec<std::path::PathBuf>) -> Result {
    for entry in fs::read_dir(folder)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let relative = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            svgs(&path, &relative, files)?;
        } else if name.to_ascii_lowercase().ends_with(".svg") {
            files.push(path);
        }
    }
    Ok(())
}

/// The pairs of `"logical name" = "path of the source"`, as `ui-kit` reads them.
fn parse_map(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with('[') {
                return None;
            }
            let (name, source) = line.split_once('=')?;
            let unquote = |text: &str| text.trim().trim_matches('"').trim().to_owned();
            let (name, source) = (unquote(name), unquote(source));
            (!name.is_empty() && !source.is_empty()).then_some((name, source))
        })
        .collect()
}

/// The SVG files of the `arro` set by their path with `/` between the folders, as text. The
/// names are compared exactly: on Windows a file that differs only in case exists, but the
/// binary and Linux do not find it.
fn arro_files(arro: &Path) -> Result<HashSet<String>> {
    let mut files = Vec::new();
    svgs(arro, "", &mut files)?;
    Ok(files
        .iter()
        .filter_map(|file| file.strip_prefix(arro).ok())
        .map(|relative| relative.to_string_lossy().replace('\\', "/"))
        .collect())
}

/// The folders of the `arro` set: the groups its icon names start with.
fn icon_groups(arro: &Path) -> Result<HashSet<String>> {
    let mut groups = HashSet::new();
    for entry in fs::read_dir(arro)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            groups.insert(entry.file_name().to_string_lossy().into_owned());
        }
    }
    Ok(groups)
}

/// The icon names the Rust sources of `crates` ask for, each with the place of its first
/// literal, as `file:line`.
fn icon_names(root: &Path, groups: &HashSet<String>) -> Result<BTreeMap<String, String>> {
    let mut files = Vec::new();
    rust_sources(&root.join("crates"), &mut files)?;
    let mut names = BTreeMap::new();
    for file in files {
        let text = fs::read_to_string(&file)?;
        let relative = file
            .strip_prefix(root)
            .unwrap_or(&file)
            .to_string_lossy()
            .replace('\\', "/");
        for (number, line) in text.lines().enumerate() {
            for name in icon_literals(line, groups) {
                names
                    .entry(name.to_owned())
                    .or_insert_with(|| format!("{relative}:{}", number + 1));
            }
        }
    }
    Ok(names)
}

/// The `"group/name"` literals of a line whose group is one of `groups`. The text between
/// two quotes is every second field of the line split at its quotes.
fn icon_literals<'a>(line: &'a str, groups: &'a HashSet<String>) -> impl Iterator<Item = &'a str> {
    line.split('"').skip(1).step_by(2).filter(move |literal| {
        literal.split_once('/').is_some_and(|(group, name)| {
            groups.contains(group) && is_name(name) && !NOT_ICONS.contains(literal)
        })
    })
}

/// Whether a name of an icon is made of letters, digits and underscores.
fn is_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_')
}

/// The file of an icon in the `arro` set, named the way `arro_file` of ui-kit names it.
fn arro_file(name: &str, dark: bool) -> String {
    let suffix = if dark && !SINGLE_LOOK.contains(&name) {
        "_dark"
    } else {
        ""
    };
    format!("{name}{suffix}.svg")
}

/// The Rust sources below `folder`.
fn rust_sources(folder: &Path, files: &mut Vec<PathBuf>) -> Result {
    for entry in fs::read_dir(folder)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            rust_sources(&path, files)?;
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            files.push(path);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    #[test]
    fn the_bundled_set_has_no_product_logos_and_names_every_file() {
        super::run(&crate::workspace_root()).unwrap();
    }

    #[test]
    fn an_icon_name_is_a_literal_of_a_group_of_the_arro_set() {
        let groups: HashSet<String> = ["general", "nodes"].into_iter().map(String::from).collect();
        let line = r#"icon("general/settings", "textDocument/hover", "general/a/b", "nodes/star", "general/")"#;
        let names: Vec<&str> = super::icon_literals(line, &groups).collect();
        assert_eq!(names, ["general/settings"]);
    }
}

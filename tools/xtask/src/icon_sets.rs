//! `check-icon-sets`: the bundled icon sets carry no product logos and keep the licence
//! line of their source. The `intellij` set names its files in `map.toml`; every file
//! there is in the set in both looks, and no other SVG is.
//!
//! The folders `welcome`, `ultimate`, `meetNewUi`, `survey` and `ide` hold the icons of
//! JetBrains products and their logos; the file names below are the logos of products or
//! companies. Neither may reach the binary.

use std::collections::HashSet;
use std::fs;
use std::path::Path;

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

#[cfg(test)]
mod tests {
    #[test]
    fn the_bundled_set_has_no_product_logos_and_names_every_file() {
        super::run(&crate::workspace_root()).unwrap();
    }
}

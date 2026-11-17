//! `check-spacing`: padding, margins and gaps in the UI crates come from the spacing
//! scale of `theme::metrics` (2, 4, 8, 12, 16 and their sums), not from odd numbers.
//! A literal such as `.p(px(6.))` or `.gap(px(10.))` fails the check; widths, heights,
//! borders and sizes of single elements are not spacing and are not checked.

use std::fs;
use std::path::{Path, PathBuf};

use crate::Result;

/// The UI crates whose sources are checked.
const CRATES: &[&str] = &[
    "crates/ui/ui-kit/src",
    "crates/ui/workspace/src",
    "crates/ui/panels/src",
];

/// Files still on the old literals, relative to the workspace root with `/`. A file
/// leaves this list when it moves to the scale; the list only shrinks.
const NOT_YET: &[&str] = &[];

/// Methods that set spacing.
const SPACING: &[&str] = &[
    "p", "px", "py", "pt", "pb", "pl", "pr", "m", "mx", "my", "mt", "mb", "ml", "mr", "gap",
    "gap_x", "gap_y",
];

/// The scale and the sums the chrome uses.
const SCALE: &[f32] = &[0., 2., 4., 8., 12., 16., 20., 24., 32.];

pub fn run(root: &Path) -> Result {
    let mut found = Vec::new();
    for dir in CRATES {
        for file in sources(&root.join(dir))? {
            let name = relative(root, &file);
            if NOT_YET.contains(&name.as_str()) {
                continue;
            }
            let text = fs::read_to_string(&file)?;
            for (line, value) in odd_spacing(&text) {
                found.push(format!(
                    "{name}:{line}: spacing {value} is not on the scale"
                ));
            }
        }
    }
    if found.is_empty() {
        return Ok(());
    }
    Err(format!(
        "{}\nuse theme::metrics::SPACE_* (2, 4, 8, 12, 16) or a metric built from them",
        found.join("\n")
    )
    .into())
}

fn sources(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_owned()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir)? {
            let path = entry?.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|ext| ext == "rs") && !is_test_file(&path) {
                out.push(path);
            }
        }
    }
    out.sort();
    Ok(out)
}

fn is_test_file(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.contains("tests"))
}

fn relative(root: &Path, file: &Path) -> String {
    file.strip_prefix(root)
        .unwrap_or(file)
        .to_string_lossy()
        .replace('\\', "/")
}

/// One-based line numbers and values of spacing literals off the scale. A module under
/// `#[cfg(test)]` ends the file for the check; a single test helper does not.
fn odd_spacing(text: &str) -> Vec<(usize, String)> {
    let mut found = Vec::new();
    let lines: Vec<&str> = text.lines().collect();
    for (index, line) in lines.iter().enumerate() {
        let next = lines.get(index + 1).map_or("", |next| next.trim_start());
        if line.trim_start().starts_with("#[cfg(test)]") && next.starts_with("mod ") {
            break;
        }
        let mut rest: &str = line;
        while let Some(at) = rest.find("(px(") {
            let before = &rest[..at];
            let method: String = before
                .chars()
                .rev()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            let is_call = before[..before.len() - method.len()].ends_with('.');
            let after = &rest[at + "(px(".len()..];
            let number: String = after
                .chars()
                .take_while(|c| c.is_ascii_digit() || *c == '.')
                .collect();
            let closed = after[number.len()..].starts_with(')');
            if is_call
                && closed
                && SPACING.contains(&method.as_str())
                && let Ok(value) = number.parse::<f32>()
                && !SCALE.contains(&value)
            {
                found.push((index + 1, number.clone()));
            }
            rest = &rest[at + "(px(".len()..];
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn odd_spacing_is_found_and_the_scale_passes() {
        let text = "div()\n  .p(px(6.))\n  .gap(px(8.))\n  .w(px(7.))\n  .px(px(10.)).my(px(4.))\n";
        assert_eq!(
            odd_spacing(text),
            vec![(2, "6.".to_owned()), (5, "10.".to_owned())]
        );
    }

    #[test]
    fn test_modules_and_constants_are_skipped() {
        let text = ".p(px(SPACE_M))\n#[cfg(test)]\nmod tests { .p(px(7.)) }\n";
        assert!(odd_spacing(text).is_empty());
    }

    #[test]
    fn the_list_of_files_not_yet_on_the_scale_names_existing_files() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        for name in NOT_YET {
            assert!(root.join(name).is_file(), "{name} does not exist");
        }
    }

    #[test]
    fn the_ui_crates_pass() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        run(&root).unwrap();
    }
}

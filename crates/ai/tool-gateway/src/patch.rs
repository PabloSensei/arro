//! The two shapes of patch an agent sends: the `*** Begin Patch` form and a unified diff.
//! Nothing here writes: the parser names the files and the new text of every one, and the
//! gateway offers each file to the user through the path of `propose_edit`.

use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PatchFile {
    /// The file the patch changes, as the patch wrote it.
    pub path: PathBuf,
    /// The new place of the file, for a patch that moves it.
    pub move_to: Option<PathBuf>,
    pub delete: bool,
    /// The lines of a new file.
    pub added: Option<Vec<String>>,
    pub hunks: Vec<PatchHunk>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PatchHunk {
    /// One-based, zero when the patch does not name a place.
    pub old_start: usize,
    pub old_lines: usize,
    pub new_start: usize,
    pub new_lines: usize,
    /// A space for a context line, `-` for one that goes, `+` for one that comes.
    pub lines: Vec<(char, String)>,
}

impl PatchFile {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            move_to: None,
            delete: false,
            added: None,
            hunks: Vec::new(),
        }
    }

    /// The text of the file after the patch.
    pub fn text(&self, base: &str) -> Result<String, String> {
        if let Some(lines) = &self.added {
            let mut text = lines.join("\n");
            if !lines.is_empty() {
                text.push('\n');
            }
            return Ok(text);
        }
        let ends_with_newline = base.ends_with('\n');
        let mut lines: Vec<String> = base.lines().map(str::to_string).collect();
        if base.is_empty() {
            lines.clear();
        }
        for hunk in &self.hunks {
            apply_hunk(&mut lines, hunk)?;
        }
        let mut text = lines.join("\n");
        if ends_with_newline && !text.is_empty() {
            text.push('\n');
        }
        Ok(text)
    }
}

fn apply_hunk(lines: &mut Vec<String>, hunk: &PatchHunk) -> Result<(), String> {
    let old: Vec<&str> = hunk
        .lines
        .iter()
        .filter(|(kind, _)| *kind != '+')
        .map(|(_, line)| line.as_str())
        .collect();
    let new: Vec<String> = hunk
        .lines
        .iter()
        .filter(|(kind, _)| *kind != '-')
        .map(|(_, line)| line.clone())
        .collect();
    let at = find(lines, &old, hunk.old_start);
    let Some(at) = at else {
        return Err(format!(
            "the hunk at line {} does not match the text of the file",
            hunk.old_start
        ));
    };
    lines.splice(at..at + old.len(), new);
    Ok(())
}

fn find(lines: &[String], old: &[&str], start: usize) -> Option<usize> {
    if old.is_empty() {
        return Some(start.saturating_sub(1).min(lines.len()));
    }
    let exact = start.saturating_sub(1);
    if exact + old.len() <= lines.len()
        && old
            .iter()
            .zip(&lines[exact..exact + old.len()])
            .all(|(left, right)| left == right)
    {
        return Some(exact);
    }
    (0..=lines.len().saturating_sub(old.len())).find(|at| {
        old.iter()
            .zip(&lines[*at..*at + old.len()])
            .all(|(left, right)| left == right)
    })
}

/// Reads a patch in either shape. A text with no file header is an error with the reason.
pub fn parse(input: &str) -> Result<Vec<PatchFile>, String> {
    let input = input.replace("\r\n", "\n");
    if input.contains("*** Begin Patch") || input.contains("*** Add File:") {
        parse_apply(&input)
    } else if input.contains("@@")
        || input.lines().any(|line| line.starts_with("--- "))
        || input.lines().any(|line| line.starts_with("diff --git "))
    {
        parse_unified(&input)
    } else {
        Err("the patch has no file header".to_string())
    }
}

fn parse_apply(input: &str) -> Result<Vec<PatchFile>, String> {
    let mut files = Vec::new();
    let mut current: Option<PatchFile> = None;
    let mut hunk: Option<PatchHunk> = None;
    let finish_hunk = |hunk: &mut Option<PatchHunk>, file: &mut Option<PatchFile>| {
        if let (Some(hunk), Some(file)) = (hunk.take(), file.as_mut()) {
            file.hunks.push(hunk);
        }
    };
    for line in input.lines() {
        if let Some(rest) = line.strip_prefix("*** Add File: ") {
            finish_hunk(&mut hunk, &mut current);
            if let Some(file) = current.take() {
                files.push(file);
            }
            let mut file = PatchFile::new(PathBuf::from(rest.trim()));
            file.added = Some(Vec::new());
            current = Some(file);
            continue;
        }
        if let Some(rest) = line.strip_prefix("*** Update File: ") {
            finish_hunk(&mut hunk, &mut current);
            if let Some(file) = current.take() {
                files.push(file);
            }
            current = Some(PatchFile::new(PathBuf::from(rest.trim())));
            continue;
        }
        if let Some(rest) = line.strip_prefix("*** Delete File: ") {
            finish_hunk(&mut hunk, &mut current);
            if let Some(file) = current.take() {
                files.push(file);
            }
            let mut file = PatchFile::new(PathBuf::from(rest.trim()));
            file.delete = true;
            current = Some(file);
            continue;
        }
        if let Some(rest) = line.strip_prefix("*** Move to: ") {
            if let Some(file) = current.as_mut() {
                file.move_to = Some(PathBuf::from(rest.trim()));
            }
            continue;
        }
        if line.starts_with("*** End Patch") || line.starts_with("*** Begin Patch") {
            finish_hunk(&mut hunk, &mut current);
            continue;
        }
        if line.starts_with("@@") {
            finish_hunk(&mut hunk, &mut current);
            hunk = Some(PatchHunk {
                old_start: hunk_start(line),
                old_lines: 0,
                new_start: 0,
                new_lines: 0,
                lines: Vec::new(),
            });
            continue;
        }
        if let Some(file) = current.as_mut()
            && let Some(lines) = file.added.as_mut()
        {
            lines.push(strip_added(line));
            continue;
        }
        if current.is_some() {
            let kind = line.chars().next().unwrap_or(' ');
            if matches!(kind, ' ' | '+' | '-') {
                let hunk = hunk.get_or_insert_with(|| PatchHunk {
                    old_start: 0,
                    old_lines: 0,
                    new_start: 0,
                    new_lines: 0,
                    lines: Vec::new(),
                });
                hunk.lines.push((kind, line[1..].to_string()));
                if kind != '+' {
                    hunk.old_lines += 1;
                }
                if kind != '-' {
                    hunk.new_lines += 1;
                }
            }
        }
    }
    finish_hunk(&mut hunk, &mut current);
    if let Some(file) = current {
        files.push(file);
    }
    if files.is_empty() {
        return Err("the patch names no file".to_string());
    }
    Ok(files)
}

fn strip_added(line: &str) -> String {
    line.strip_prefix('+').unwrap_or(line).to_string()
}

fn hunk_start(line: &str) -> usize {
    line.split_whitespace()
        .find_map(|word| word.strip_prefix('-'))
        .and_then(|range| range.split(',').next())
        .and_then(|number| number.parse().ok())
        .unwrap_or(0)
}

fn parse_unified(input: &str) -> Result<Vec<PatchFile>, String> {
    let mut files: Vec<PatchFile> = Vec::new();
    let mut current: Option<PatchFile> = None;
    let mut hunk: Option<PatchHunk> = None;
    let mut deleted = false;
    let finish_hunk = |hunk: &mut Option<PatchHunk>, current: &mut Option<PatchFile>| {
        if let (Some(hunk), Some(file)) = (hunk.take(), current.as_mut()) {
            file.hunks.push(hunk);
        }
    };
    for line in input.lines() {
        if line.starts_with("diff --git ") {
            finish_hunk(&mut hunk, &mut current);
            if let Some(file) = current.take() {
                files.push(file);
            }
            deleted = false;
            continue;
        }
        if line.starts_with("new file mode") {
            deleted = false;
            continue;
        }
        if line.starts_with("deleted file mode") {
            deleted = true;
            continue;
        }
        if let Some(rest) = line.strip_prefix("rename from ") {
            if let Some(file) = current.as_mut() {
                file.path = PathBuf::from(rest.trim());
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix("rename to ") {
            if let Some(file) = current.as_mut() {
                file.move_to = Some(PathBuf::from(rest.trim()));
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix("--- ") {
            finish_hunk(&mut hunk, &mut current);
            if let Some(file) = current.take() {
                files.push(file);
            }
            let path = diff_path(rest);
            let mut file = PatchFile::new(path);
            file.delete = deleted;
            current = Some(file);
            continue;
        }
        if let Some(rest) = line.strip_prefix("+++ ") {
            let path = diff_path(rest);
            if let Some(file) = current.as_mut()
                && path.as_os_str() != "/dev/null"
                && file.path.as_os_str() == "/dev/null"
            {
                file.path = path;
            }
            continue;
        }
        if let Some(heading) = line.strip_prefix("@@ ") {
            finish_hunk(&mut hunk, &mut current);
            let (old_start, old_lines, new_start, new_lines) = parse_heading(heading)?;
            hunk = Some(PatchHunk {
                old_start,
                old_lines,
                new_start,
                new_lines,
                lines: Vec::new(),
            });
            continue;
        }
        if let Some(hunk) = hunk.as_mut() {
            let kind = line.chars().next().unwrap_or(' ');
            if matches!(kind, ' ' | '+' | '-')
                && !line.starts_with("+++")
                && !line.starts_with("---")
            {
                hunk.lines.push((kind, line[1..].to_string()));
            }
        }
    }
    finish_hunk(&mut hunk, &mut current);
    if let Some(file) = current {
        files.push(file);
    }
    if files.is_empty() {
        return Err("the patch names no file".to_string());
    }
    Ok(files)
}

fn diff_path(rest: &str) -> PathBuf {
    let path = rest.split('\t').next().unwrap_or(rest).trim();
    let path = path
        .strip_prefix("a/")
        .or_else(|| path.strip_prefix("b/"))
        .unwrap_or(path);
    PathBuf::from(path)
}

fn parse_heading(heading: &str) -> Result<(usize, usize, usize, usize), String> {
    let heading = heading.split("@@").next().unwrap_or(heading).trim();
    let mut parts = heading.split_whitespace();
    let old = parts
        .next()
        .and_then(|part| part.strip_prefix('-'))
        .ok_or_else(|| "the hunk has no old range".to_string())?;
    let new = parts
        .next()
        .and_then(|part| part.strip_prefix('+'))
        .ok_or_else(|| "the hunk has no new range".to_string())?;
    let (old_start, old_lines) = range(old);
    let (new_start, new_lines) = range(new);
    Ok((old_start, old_lines, new_start, new_lines))
}

fn range(text: &str) -> (usize, usize) {
    let mut parts = text.split(',');
    let start = parts.next().and_then(|part| part.parse().ok()).unwrap_or(0);
    let lines = parts.next().and_then(|part| part.parse().ok()).unwrap_or(1);
    (start, lines)
}

/// Rejects a path that would leave the project.
pub fn path_inside(project: &Path, path: &Path) -> Result<PathBuf, String> {
    let mut resolved = PathBuf::new();
    for component in project.join(path).components() {
        match component {
            std::path::Component::ParentDir => {
                resolved.pop();
            }
            std::path::Component::CurDir => {}
            other => resolved.push(other),
        }
    }
    let lower = |path: &Path| PathBuf::from(path.to_string_lossy().to_lowercase());
    if !lower(&resolved).starts_with(lower(project)) {
        return Err(format!("{} is outside the project", path.display()));
    }
    Ok(resolved)
}

/// A one-line picture of a patch for the question of the user.
pub fn summary(files: &[PatchFile]) -> String {
    let names: Vec<String> = files
        .iter()
        .map(|file| {
            let action = if file.delete {
                "delete"
            } else if file.added.is_some() {
                "add"
            } else {
                "update"
            };
            format!("{action} {}", file.path.display())
        })
        .collect();
    names.join(", ")
}

#[cfg(test)]
mod tests {
    use super::{parse, path_inside};
    use std::path::Path;

    #[test]
    fn a_unified_diff_gives_the_new_text() {
        let patch = "\
--- a/src/Main.txt
+++ b/src/Main.txt
@@ -1,3 +1,3 @@
 one
-two
+TWO
 three
";
        let files = parse(patch).expect("a patch");
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, Path::new("src/Main.txt"));
        assert_eq!(
            files[0].text("one\ntwo\nthree\n").expect("text"),
            "one\nTWO\nthree\n"
        );
    }

    #[test]
    fn the_apply_patch_form_adds_and_updates() {
        let patch = "\
*** Begin Patch
*** Add File: src/New.txt
+hello
+world
*** Update File: src/Main.txt
@@
-one
+ONE
*** Delete File: src/Old.txt
*** Move to: src/Moved.txt
*** End Patch
";
        let files = parse(patch).expect("a patch");
        assert_eq!(files.len(), 3);
        assert_eq!(files[0].text("").expect("added"), "hello\nworld\n");
        assert_eq!(files[1].text("one\n").expect("updated"), "ONE\n");
        assert!(files[2].delete);
        assert_eq!(
            files[2].move_to.as_deref(),
            Some(Path::new("src/Moved.txt"))
        );
    }

    #[test]
    fn a_path_outside_the_project_is_refused() {
        assert!(path_inside(Path::new("project"), Path::new("../other/a.txt")).is_err());
        assert!(path_inside(Path::new("project"), Path::new("src/a.txt")).is_ok());
    }
}

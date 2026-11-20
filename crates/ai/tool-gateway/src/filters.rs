//! File masks of the tools: a glob for the file search and the `paths` list of the
//! search tools, where a leading `!` excludes a place.

/// Whether a relative path with `/` matches a glob such as `**.rs` or `src/*/main.*`.
/// A pattern without a separator matches the file name alone.
pub fn matches_glob(pattern: &str, path: &str) -> bool {
    let pattern = pattern.trim().trim_start_matches('/');
    if pattern.is_empty() || pattern == "*" || pattern == "**" {
        return true;
    }
    let name = path.rsplit('/').next().unwrap_or(path);
    if !pattern.contains('/') {
        return wildcard(pattern, name);
    }
    let pattern: Vec<&str> = pattern.split('/').collect();
    let path: Vec<&str> = path.split('/').collect();
    segments(&pattern, &path)
}

fn wildcard(pattern: &str, text: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let text: Vec<char> = text.chars().collect();
    let mut pattern_at = 0;
    let mut text_at = 0;
    let mut star = None;
    let mut after = 0;
    while text_at < text.len() {
        if pattern_at < pattern.len()
            && (pattern[pattern_at] == '?' || pattern[pattern_at] == text[text_at])
        {
            pattern_at += 1;
            text_at += 1;
        } else if pattern_at < pattern.len() && pattern[pattern_at] == '*' {
            star = Some(pattern_at);
            after = text_at;
            pattern_at += 1;
        } else if let Some(star) = star {
            pattern_at = star + 1;
            after += 1;
            text_at = after;
        } else {
            return false;
        }
    }
    pattern[pattern_at..]
        .iter()
        .all(|character| *character == '*')
}

fn segments(pattern: &[&str], path: &[&str]) -> bool {
    match pattern.split_first() {
        None => path.is_empty(),
        Some((&"**", rest)) => {
            if segments(rest, path) {
                return true;
            }
            (0..path.len()).any(|skip| segments(rest, &path[skip + 1..]))
        }
        Some((first, rest)) => match path.split_first() {
            Some((name, path)) if wildcard(first, name) => segments(rest, path),
            _ => false,
        },
    }
}

/// Whether `paths` lets a relative path through. A positive entry names a folder or a
/// glob, a `!` entry excludes one; without positive entries everything is taken.
pub fn path_allowed(paths: &[String], relative: &str) -> bool {
    let relative = relative.trim_start_matches('/');
    let trimmed = relative.trim_end_matches('/');
    if paths
        .iter()
        .filter_map(|entry| entry.trim().strip_prefix('!'))
        .any(|entry| names(entry, trimmed))
    {
        return false;
    }
    let positives: Vec<&str> = paths
        .iter()
        .map(|entry| entry.trim())
        .filter(|entry| !entry.starts_with('!'))
        .filter(|entry| !entry.is_empty())
        .collect();
    positives.is_empty() || positives.iter().any(|entry| names(entry, trimmed))
}

fn names(pattern: &str, relative: &str) -> bool {
    let pattern = pattern.trim().trim_matches('/');
    if pattern.is_empty() {
        return true;
    }
    if pattern.contains('*') || pattern.contains('?') {
        return matches_glob(pattern, relative);
    }
    relative == pattern
        || relative.starts_with(&format!("{pattern}/"))
        || relative.rsplit('/').next() == Some(pattern)
}

#[cfg(test)]
mod tests {
    use super::{matches_glob, path_allowed};

    #[test]
    fn a_glob_matches_names_and_folders() {
        assert!(matches_glob("**/*.txt", "src/Main.txt"));
        assert!(matches_glob("**/*.txt", "Main.txt"));
        assert!(matches_glob("src/**", "src/a/b/Main.txt"));
        assert!(matches_glob("src/*/Main.txt", "src/a/Main.txt"));
        assert!(!matches_glob("src/*/Main.txt", "src/a/b/Main.txt"));
        assert!(matches_glob("Main.?xt", "Main.txt"));
        assert!(matches_glob("*", "Main.txt"));
    }

    #[test]
    fn the_paths_list_takes_and_excludes() {
        let paths = vec![
            "src".to_string(),
            "!src/generated".to_string(),
            "**/*.txt".to_string(),
        ];
        assert!(path_allowed(&paths, "src/Main.txt"));
        assert!(!path_allowed(&paths, "src/generated/Main.txt"));
        assert!(path_allowed(&paths, "notes.txt"));
        assert!(!path_allowed(&paths, "other/Main.rs"));
        assert!(path_allowed(&[], "anything/at/all.rs"));
    }
}

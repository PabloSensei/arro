//! Go to File and the files of Search Everywhere.

use std::path::{Path, PathBuf};

use crate::fuzzy::{FuzzyMatcher, NameMatcher, Ranked, rank};

/// What the user typed, with an optional place: `Owner.src:12:5` or `Owner:12`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GotoQuery {
    pub pattern: String,
    /// 1-based, as typed.
    pub line: Option<u32>,
    pub column: Option<u32>,
}

impl GotoQuery {
    pub fn parse(text: &str) -> Self {
        let text = text.trim();
        let split = |text: &str| {
            let (head, tail) = text.rsplit_once(':')?;
            Some((head.to_owned(), tail.parse::<u32>().ok()?))
        };
        let (pattern, line, column) = match split(text) {
            None => (text.to_owned(), None, None),
            Some((head, last)) => match split(&head) {
                Some((pattern, line)) => (pattern, Some(line), Some(last)),
                None => (head, Some(last), None),
            },
        };
        Self {
            pattern,
            line,
            column,
        }
    }
}

fn slashes(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// Ranks `files` by name, or by the path under `root` when the pattern has a slash.
pub fn rank_files<'a>(
    pattern: &str,
    root: &Path,
    files: impl IntoIterator<Item = &'a Path>,
    limit: usize,
) -> Vec<Ranked<PathBuf>> {
    let by_path = pattern.contains(['/', '\\']);
    let pattern = pattern.replace('\\', "/");
    let mut matcher = if by_path {
        NameMatcher::for_paths()
    } else {
        NameMatcher::new()
    };
    let keyed = files.into_iter().map(|path| {
        let key = if by_path {
            slashes(path.strip_prefix(root).unwrap_or(path))
        } else {
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default()
        };
        (key, path)
    });
    let matcher: &mut dyn FuzzyMatcher = &mut matcher;
    rank(matcher, &pattern, keyed, |(key, _)| key, limit)
        .into_iter()
        .map(|ranked| Ranked {
            item: ranked.item.1.to_path_buf(),
            score: ranked.score,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_place_after_the_name_is_read() {
        let query = GotoQuery::parse("Owner.src:12:5");
        assert_eq!(query.pattern, "Owner.src");
        assert_eq!((query.line, query.column), (Some(12), Some(5)));

        let query = GotoQuery::parse("Owner:7");
        assert_eq!((query.pattern.as_str(), query.line), ("Owner", Some(7)));

        let query = GotoQuery::parse("Owner");
        assert_eq!((query.pattern.as_str(), query.line), ("Owner", None));

        let query = GotoQuery::parse("C:");
        assert_eq!((query.pattern.as_str(), query.line), ("C:", None));
    }

    #[test]
    fn names_match_by_default_and_paths_with_a_slash() {
        let root = Path::new("/p");
        let files = [
            Path::new("/p/src/main/owner/Owner.src"),
            Path::new("/p/src/main/vet/Vet.src"),
            Path::new("/p/src/test/owner/OwnerTests.src"),
        ];
        let found = rank_files("owner", root, files, 10);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].item, files[0]);

        let found = rank_files("test/own", root, files, 10);
        assert_eq!(found[0].item, files[2]);
    }
}

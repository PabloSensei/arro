//! How far one word is from another, and which words a typo was meant to be.

use std::collections::HashSet;

/// The Damerau–Levenshtein distance of two words with neighbouring transpositions
/// counted once, which is what a mistyped pair of letters costs a user.
pub fn distance(left: &str, right: &str) -> usize {
    let left: Vec<char> = left.chars().collect();
    let right: Vec<char> = right.chars().collect();
    if left.is_empty() {
        return right.len();
    }
    if right.is_empty() {
        return left.len();
    }
    let mut rows = vec![vec![0usize; right.len() + 1]; left.len() + 1];
    for (index, row) in rows.iter_mut().enumerate() {
        row[0] = index;
    }
    for (index, cell) in rows[0].iter_mut().enumerate() {
        *cell = index;
    }
    for i in 1..=left.len() {
        for j in 1..=right.len() {
            let substitution = usize::from(left[i - 1] != right[j - 1]);
            let mut best = (rows[i - 1][j] + 1)
                .min(rows[i][j - 1] + 1)
                .min(rows[i - 1][j - 1] + substitution);
            if i > 1 && j > 1 && left[i - 1] == right[j - 2] && left[i - 2] == right[j - 1] {
                best = best.min(rows[i - 2][j - 2] + 1);
            }
            rows[i][j] = best;
        }
    }
    rows[left.len()][right.len()]
}

/// How many edits a word of this length may be away from its correction.
pub fn reach(word: &str) -> usize {
    match word.chars().count() {
        0..=4 => 1,
        5..=8 => 2,
        _ => 3,
    }
}

/// The closest words of `candidates` to `word`, at most `limit` of them, nearest
/// first and alphabetical among equals. A word at distance zero is left out.
pub fn nearest<'a>(
    word: &str,
    candidates: impl IntoIterator<Item = &'a str>,
    limit: usize,
) -> Vec<String> {
    let reach = reach(word);
    let needle = word.to_lowercase();
    let mut seen = HashSet::new();
    let mut found: Vec<(usize, String)> = Vec::new();
    for candidate in candidates {
        let lower = candidate.to_lowercase();
        if lower == needle || !seen.insert(lower.clone()) {
            continue;
        }
        if lower.chars().count().abs_diff(needle.chars().count()) > reach {
            continue;
        }
        let found_distance = distance(&needle, &lower);
        if found_distance <= reach {
            found.push((found_distance, candidate.to_owned()));
        }
    }
    found.sort_by(|left, right| {
        left.0
            .cmp(&right.0)
            .then_with(|| left.1.to_lowercase().cmp(&right.1.to_lowercase()))
    });
    found.truncate(limit);
    found.into_iter().map(|(_, word)| word).collect()
}

/// Gives `word` the capital of the typo it replaces, so `Calcualte` becomes
/// `Calculate` and not `calculate`.
pub fn match_case(word: &str, typo: &str) -> String {
    if typo.chars().next().is_some_and(char::is_uppercase) {
        let mut characters = word.chars();
        match characters.next() {
            Some(first) => first.to_uppercase().chain(characters).collect(),
            None => String::new(),
        }
    } else {
        word.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_distance_counts_a_swap_as_one_edit() {
        assert_eq!(distance("calcualte", "calculate"), 1);
        assert_eq!(distance("server", "server"), 0);
        assert_eq!(distance("", "abc"), 3);
        assert_eq!(distance("abc", ""), 3);
    }

    #[test]
    fn the_nearest_words_come_by_distance_and_then_by_name() {
        let candidates = ["calibrate", "calculate", "calculation", "client"];
        let found = nearest("calcualte", candidates, 3);
        assert_eq!(found[0], "calculate");
        assert!(found.len() <= 3);
        assert!(!found.contains(&"client".to_owned()));

        let tied = nearest("server", ["serger", "serves", "serveer"], 5);
        assert_eq!(tied.first().map(String::as_str), Some("serger"));
        assert!(
            tied.windows(2)
                .all(|pair| distance("server", &pair[0]) <= distance("server", &pair[1])),
            "the list is ordered by distance: {tied:?}"
        );
    }

    #[test]
    fn a_capital_of_the_typo_stays_on_the_correction() {
        assert_eq!(match_case("calculate", "Calcualte"), "Calculate");
        assert_eq!(match_case("calculate", "calcualte"), "calculate");
    }
}

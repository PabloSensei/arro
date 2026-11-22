//! Fuzzy matching of names. `nucleo-matcher` does the matching, CamelHumps on top lifts
//! the candidates where every word of the pattern starts a word of the name, the way
//! `PCC` finds `PetClinicController` first.

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

/// Higher scores are better. Indices count characters of the candidate.
pub trait FuzzyMatcher {
    fn set_pattern(&mut self, pattern: &str);
    fn score(&mut self, candidate: &str) -> Option<u32>;
    fn indices(&mut self, candidate: &str, indices: &mut Vec<u32>) -> Option<u32>;
    /// The kind of the match and its score, the way [`NameMatcher::classify`] gives them.
    fn classify(&mut self, candidate: &str) -> Option<(MatchKind, u32)>;
}

/// How a query matches a candidate, the best kinds first. A list that has matches of a
/// better kind hides those of a worse one, so that scattered letters do not crowd out
/// the names the user sees.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum MatchKind {
    /// The whole name is the query.
    Exact,
    /// The name begins with the query.
    Prefix,
    /// Every word of the query starts a word of the name, `tw` for Tool Window.
    WordStarts,
    /// The query stands in the name as written, anywhere.
    Substring,
    /// The letters of the query are in the name, with others between them.
    Scattered,
}

/// Above any score nucleo gives a name of ordinary length.
const HUMPS_BONUS: u32 = 1 << 12;
const EXACT_BONUS: u32 = 1 << 14;
const PREFIX_BONUS: u32 = 1 << 13;

pub struct NameMatcher {
    matcher: Matcher,
    pattern: Pattern,
    raw: String,
    /// The pattern cut at its humps, lower case: `PetCC` gives `pet`, `c`, `c`.
    humps: Vec<String>,
    buffer: Vec<char>,
}

impl NameMatcher {
    pub fn new() -> Self {
        Self::with_config(Config::DEFAULT)
    }

    /// Path separators count as word starts.
    pub fn for_paths() -> Self {
        Self::with_config(Config::DEFAULT.match_paths())
    }

    fn with_config(config: Config) -> Self {
        Self {
            matcher: Matcher::new(config),
            pattern: Pattern::default(),
            raw: String::new(),
            humps: Vec::new(),
            buffer: Vec::new(),
        }
    }

    fn bonus(&self, candidate: &str) -> u32 {
        let mut bonus = 0;
        if candidate.eq_ignore_ascii_case(&self.raw) {
            bonus += EXACT_BONUS;
        } else if starts_with_ignore_case(candidate, &self.raw) {
            bonus += PREFIX_BONUS;
        }
        if !self.humps.is_empty() && matches_humps(&self.humps, candidate) {
            bonus += HUMPS_BONUS;
        }
        bonus
    }

    /// The kind of the match of the pattern last given to [`FuzzyMatcher::set_pattern`],
    /// and its score. `None` when the pattern does not match the candidate at all.
    pub fn classify(&mut self, candidate: &str) -> Option<(MatchKind, u32)> {
        let score = self.score(candidate)?;
        Some((self.kind(candidate), score))
    }

    /// Whether the query spells a word of the name in full, not just its beginning.
    /// `whitespace` spells a word of `Show whitespace` but not of `Show Whitespaces`.
    pub fn spells_word(&self, candidate: &str) -> bool {
        spells_word(&self.raw, candidate)
    }

    fn kind(&self, candidate: &str) -> MatchKind {
        if self.raw.is_empty() || candidate.eq_ignore_ascii_case(&self.raw) {
            MatchKind::Exact
        } else if starts_with_ignore_case(candidate, &self.raw) {
            MatchKind::Prefix
        } else if matches_words(&self.raw, candidate) {
            MatchKind::WordStarts
        } else if contains_ignore_case(candidate, &self.raw) {
            MatchKind::Substring
        } else {
            MatchKind::Scattered
        }
    }
}

impl Default for NameMatcher {
    fn default() -> Self {
        Self::new()
    }
}

impl FuzzyMatcher for NameMatcher {
    fn set_pattern(&mut self, pattern: &str) {
        let pattern = pattern.trim();
        self.raw = pattern.to_owned();
        self.pattern = Pattern::parse(pattern, CaseMatching::Ignore, Normalization::Smart);
        self.humps = split_humps(pattern);
    }

    fn score(&mut self, candidate: &str) -> Option<u32> {
        let haystack = Utf32Str::new(candidate, &mut self.buffer);
        let score = self.pattern.score(haystack, &mut self.matcher)?;
        Some(score + self.bonus(candidate))
    }

    fn indices(&mut self, candidate: &str, indices: &mut Vec<u32>) -> Option<u32> {
        indices.clear();
        let haystack = Utf32Str::new(candidate, &mut self.buffer);
        let score = self.pattern.indices(haystack, &mut self.matcher, indices)?;
        indices.sort_unstable();
        indices.dedup();
        Some(score + self.bonus(candidate))
    }

    fn classify(&mut self, candidate: &str) -> Option<(MatchKind, u32)> {
        NameMatcher::classify(self, candidate)
    }
}

fn starts_with_ignore_case(text: &str, prefix: &str) -> bool {
    let mut text = text.chars().flat_map(char::to_lowercase);
    prefix
        .chars()
        .flat_map(char::to_lowercase)
        .all(|wanted| text.next() == Some(wanted))
}

fn contains_ignore_case(text: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    let text: Vec<char> = text.chars().flat_map(char::to_lowercase).collect();
    let needle: Vec<char> = needle.chars().flat_map(char::to_lowercase).collect();
    text.windows(needle.len())
        .any(|window| window == needle.as_slice())
}

/// Every word of the query starts a word of the name, in order: `tw` matches Tool
/// Window and `PCC` matches PetClinicController.
fn matches_words(pattern: &str, name: &str) -> bool {
    let words = name_words(name);
    if words.is_empty() {
        return false;
    }
    let segments: Vec<String> = pattern
        .split_whitespace()
        .map(|word| word.chars().flat_map(char::to_lowercase).collect())
        .filter(|word: &String| !word.is_empty())
        .collect();
    if segments.is_empty() {
        return false;
    }
    if fits_words(&segments, &words) {
        return true;
    }
    // A word of the query may carry its own humps: `PetCC` is Pet, C, C.
    let humps: Vec<String> = pattern.split_whitespace().flat_map(split_humps).collect();
    humps.len() > segments.len() && fits_words(&humps, &words)
}

/// The words of a name: a separator, a space, an upper case letter or a digit run
/// starts one. Unlike [`word_starts`], a word after a space starts there whatever its
/// case, so `Show whitespace` has two words.
fn name_words(name: &str) -> Vec<Vec<char>> {
    let chars: Vec<char> = name.chars().collect();
    let mut words: Vec<Vec<char>> = Vec::new();
    let mut current: Vec<char> = Vec::new();
    for (index, &character) in chars.iter().enumerate() {
        if character.is_whitespace() || is_separator(character) {
            if !current.is_empty() {
                words.push(std::mem::take(&mut current));
            }
            continue;
        }
        let boundary = match index.checked_sub(1).map(|before| chars[before]) {
            None => true,
            Some(before) if before.is_whitespace() || is_separator(before) => true,
            Some(before) => {
                (character.is_uppercase() && !before.is_uppercase())
                    || (character.is_ascii_digit() && !before.is_ascii_digit())
            }
        };
        if boundary && !current.is_empty() {
            words.push(std::mem::take(&mut current));
        }
        current.push(character.to_lowercase().next().unwrap_or(character));
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

fn fits_words(parts: &[String], words: &[Vec<char>]) -> bool {
    fits_from(parts, words, 0)
}

fn fits_from(parts: &[String], words: &[Vec<char>], at: usize) -> bool {
    let Some((part, rest)) = parts.split_first() else {
        return true;
    };
    let part: Vec<char> = part.chars().collect();
    if part.is_empty() {
        return fits_from(rest, words, at);
    }
    for start in at..words.len() {
        if words[start].starts_with(&part) && fits_from(rest, words, start + 1) {
            return true;
        }
        if start + part.len() <= words.len()
            && part
                .iter()
                .enumerate()
                .all(|(offset, letter)| words[start + offset].first() == Some(letter))
            && fits_from(rest, words, start + part.len())
        {
            return true;
        }
    }
    false
}

/// Whether the query spells whole words of the name, not only their beginnings.
fn spells_word(pattern: &str, name: &str) -> bool {
    let words = name_words(name);
    let segments: Vec<Vec<char>> = pattern
        .split_whitespace()
        .map(|word| word.chars().flat_map(char::to_lowercase).collect())
        .filter(|word: &Vec<char>| !word.is_empty())
        .collect();
    !segments.is_empty() && segments.iter().all(|segment| words.contains(segment))
}

/// Words of the pattern: an upper case letter, a digit run or a separator starts one.
fn split_humps(pattern: &str) -> Vec<String> {
    let mut humps: Vec<String> = Vec::new();
    for (index, character) in pattern.chars().enumerate() {
        if is_separator(character) || character == ' ' || character == '*' {
            humps.push(String::new());
            continue;
        }
        if index == 0 || character.is_uppercase() || humps.is_empty() {
            humps.push(String::new());
        }
        if let Some(last) = humps.last_mut() {
            last.extend(character.to_lowercase());
        }
    }
    humps.retain(|hump| !hump.is_empty());
    humps
}

fn is_separator(character: char) -> bool {
    matches!(character, '.' | '_' | '-' | '/' | '\\' | '$')
}

/// Where words of `name` start, as character indices.
fn word_starts(name: &[char]) -> Vec<usize> {
    let mut starts = Vec::new();
    for (index, &character) in name.iter().enumerate() {
        if is_separator(character) {
            continue;
        }
        let start = match index.checked_sub(1).map(|before| name[before]) {
            None => true,
            Some(before) if is_separator(before) => true,
            Some(before) => {
                (character.is_uppercase() && !before.is_uppercase())
                    || (character.is_ascii_digit() && !before.is_ascii_digit())
            }
        };
        if start {
            starts.push(index);
        }
    }
    starts
}

/// Each hump is a prefix of a word of `name`, in order, words may be skipped. The last
/// hump may also run on into the next words: `PetClin` and `PCont` both fit.
fn matches_humps(humps: &[String], name: &str) -> bool {
    let name: Vec<char> = name.chars().collect();
    let lower: Vec<char> = name
        .iter()
        .map(|character| character.to_lowercase().next().unwrap_or(*character))
        .collect();
    let starts = word_starts(&name);
    fit(humps, &lower, &starts, 0)
}

fn fit(humps: &[String], lower: &[char], starts: &[usize], from: usize) -> bool {
    let Some((hump, rest)) = humps.split_first() else {
        return true;
    };
    let hump: Vec<char> = hump.chars().collect();
    starts.iter().filter(|&&start| start >= from).any(|&start| {
        let end = start + hump.len();
        end <= lower.len() && lower[start..end] == hump[..] && fit(rest, lower, starts, end)
    })
}

/// One candidate that matched.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ranked<T> {
    pub item: T,
    pub score: u32,
    /// How the query matched the key, the better kinds first.
    pub kind: MatchKind,
}

/// The best `limit` items, best first: the better kind of a match, then the score, then
/// the shorter key, then alphabetically.
pub fn rank<T>(
    matcher: &mut dyn FuzzyMatcher,
    pattern: &str,
    items: impl IntoIterator<Item = T>,
    key: impl Fn(&T) -> &str,
    limit: usize,
) -> Vec<Ranked<T>> {
    matcher.set_pattern(pattern);
    let mut found: Vec<Ranked<T>> = items
        .into_iter()
        .filter_map(|item| {
            let (kind, score) = matcher.classify(key(&item))?;
            Some(Ranked { item, score, kind })
        })
        .collect();
    found.sort_by(|a, b| {
        let (left, right) = (key(&a.item), key(&b.item));
        a.kind
            .cmp(&b.kind)
            .then_with(|| b.score.cmp(&a.score))
            .then(left.len().cmp(&right.len()))
            .then_with(|| left.cmp(right))
    });
    found.truncate(limit);
    found
}

/// Drops the matches of scattered letters when a row matches better: a list does not show
/// the letters spread over a long name beside the names that begin with the query.
pub fn retain_better_kinds<T>(rows: &mut Vec<Ranked<T>>) {
    if rows.iter().any(|row| row.kind < MatchKind::Scattered) {
        rows.retain(|row| row.kind < MatchKind::Scattered);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn best(pattern: &str, names: &[&'static str]) -> Vec<&'static str> {
        let mut matcher = NameMatcher::new();
        rank(
            &mut matcher,
            pattern,
            names.iter().copied(),
            |name| name,
            10,
        )
        .into_iter()
        .map(|ranked| ranked.item)
        .collect()
    }

    #[test]
    fn humps_split_on_capitals_and_separators() {
        assert_eq!(split_humps("PetCC"), ["pet", "c", "c"]);
        assert_eq!(split_humps("pet.cont"), ["pet", "cont"]);
        assert_eq!(split_humps("owner"), ["owner"]);
    }

    #[test]
    fn camel_humps_come_first() {
        let names = [
            "PrinterConfigCache",
            "PetClinicController",
            "PaymentCancelledCommand",
            "ApplicationPccHelper",
        ];
        let found = best("PCC", &names);
        assert_eq!(found.len(), 4);
        assert!(
            found[..3]
                .iter()
                .all(|name| *name != "ApplicationPccHelper")
        );
        assert_eq!(best("PetCC", &names)[0], "PetClinicController");
        assert_eq!(best("PetClin", &names)[0], "PetClinicController");
    }

    #[test]
    fn exact_name_beats_longer_ones() {
        let found = best("owner", &["OwnerController", "Owner", "PetOwnerRepository"]);
        assert_eq!(found, ["Owner", "OwnerController", "PetOwnerRepository"]);
    }

    #[test]
    fn indices_mark_the_matched_characters() {
        let mut matcher = NameMatcher::new();
        matcher.set_pattern("oc");
        let mut indices = Vec::new();
        matcher.indices("OwnerController", &mut indices).unwrap();
        assert_eq!(indices, [0, 5]);
        assert_eq!(matcher.score("Vet"), None);
    }

    fn kind(pattern: &str, name: &str) -> Option<MatchKind> {
        let mut matcher = NameMatcher::new();
        matcher.set_pattern(pattern);
        matcher.classify(name).map(|(kind, _)| kind)
    }

    #[test]
    fn match_kinds_run_from_the_exact_name_to_scattered_letters() {
        assert_eq!(kind("todo", "TODO"), Some(MatchKind::Exact));
        assert_eq!(kind("todo", "TODO Tool Window"), Some(MatchKind::Prefix));
        assert_eq!(kind("tw", "Tool Window"), Some(MatchKind::WordStarts));
        assert_eq!(
            kind("pcc", "PetClinicController"),
            Some(MatchKind::WordStarts)
        );
        assert_eq!(
            kind("petcc", "PetClinicController"),
            Some(MatchKind::Scattered)
        );
        assert_eq!(
            kind("PetCC", "PetClinicController"),
            Some(MatchKind::WordStarts)
        );
        assert_eq!(kind("indow", "Tool Window"), Some(MatchKind::Substring));
        assert_eq!(
            kind("tdc", "Toggle rendered doc"),
            Some(MatchKind::Scattered)
        );
        assert_eq!(
            kind("todo", "Services"),
            None,
            "the letters are not all there"
        );
    }

    #[test]
    fn word_starts_need_every_word_of_the_query() {
        assert_eq!(
            kind("todo", "Toggle rendered doc"),
            Some(MatchKind::Scattered),
            "the letters do not begin the words"
        );
        assert_eq!(kind("tw", "TODO Tool Window"), Some(MatchKind::WordStarts));
    }

    #[test]
    fn a_spelled_word_is_not_a_prefix_of_a_longer_one() {
        let mut matcher = NameMatcher::new();
        matcher.set_pattern("whitespace");
        assert!(matcher.spells_word("Show whitespace"));
        assert!(!matcher.spells_word("Show Whitespaces"));
        assert!(matcher.spells_word("whitespace and tabs"));
    }
}

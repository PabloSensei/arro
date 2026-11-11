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
}

fn starts_with_ignore_case(text: &str, prefix: &str) -> bool {
    let mut text = text.chars().flat_map(char::to_lowercase);
    prefix
        .chars()
        .flat_map(char::to_lowercase)
        .all(|wanted| text.next() == Some(wanted))
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
}

/// The best `limit` items, best first. Ties go to the shorter key, then alphabetically.
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
            let score = matcher.score(key(&item))?;
            Some(Ranked { item, score })
        })
        .collect();
    found.sort_by(|a, b| {
        let (left, right) = (key(&a.item), key(&b.item));
        b.score
            .cmp(&a.score)
            .then(left.len().cmp(&right.len()))
            .then_with(|| left.cmp(right))
    });
    found.truncate(limit);
    found
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
}

//! A Unix system without AppKit has no spell checker to ask; the words of a hunspell
//! dictionary, when the machine has one, are read by the caller instead.

pub fn spelling_supported(_language: &str) -> bool {
    false
}

pub fn spelling_source(_language: &str) -> Option<String> {
    None
}

pub fn spelling_check(_language: &str, _word: &str) -> Option<bool> {
    None
}

pub fn spelling_suggest(_language: &str, _word: &str, _limit: usize) -> Vec<String> {
    Vec::new()
}

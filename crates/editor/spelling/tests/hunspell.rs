//! The hunspell files of the tests: a small dictionary with one suffix flag, read from
//! the disk the way the dictionary of a Unix system is.

use std::fs;
use std::path::PathBuf;

use spelling::Dictionary;

fn data(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("data")
        .join(name)
}

#[test]
fn a_small_hunspell_pair_gives_its_stems_and_its_suffix_forms() {
    let dic = fs::read_to_string(data("en_US.dic")).unwrap();
    let aff = fs::read_to_string(data("en_US.aff")).unwrap();
    let dictionary = Dictionary::from_hunspell(&dic, &aff);

    for word in [
        "calculate",
        "calculates",
        "server",
        "servers",
        "config",
        "configs",
    ] {
        assert!(dictionary.contains(word), "{word} is a word of the file");
    }
    for word in ["calcualte", "serverr", "configes"] {
        assert!(!dictionary.contains(word), "{word} is no word of the file");
    }
}

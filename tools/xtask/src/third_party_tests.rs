//! The real `THIRD-PARTY.txt` of the binary: every crate in the graph of `ide` with a license,
//! plus the assets and the trademark text that are not crates.

use std::path::Path;

use crate::dist;

#[test]
fn every_crate_of_the_binary_has_a_license_and_the_assets_are_named() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let text = dist::third_party(&root).unwrap();
    assert!(!text.contains("UNKNOWN"), "{text}");
    assert!(text.contains("BUNDLED ASSETS"));
    assert!(text.contains("Inter 4.1"));
    assert!(text.contains("JetBrains Mono 2.304"));
    assert!(text.contains("Apache-2.0"));
    assert!(text.contains("EPL-2.0"));
    assert!(text.contains("EPL-1.0"));
    assert!(text.contains("MIT-licensed"));
    assert!(text.contains("trademarks of JetBrains"));
    assert!(text.contains("no product logos"));
}

//! Checks from qa: a restore keeps the file it replaces, one big file leaves the rest alone.
use std::fs;
use std::time::Duration;

use local_history::{Cause, Limits, LocalHistory};

fn temp(name: &str) -> std::path::PathBuf {
    test_support::scratch(&format!("qa-lh-{name}"))
}

#[test]
fn restore_overwrites_a_file_that_came_back_without_keeping_it() {
    let root = temp("restore");
    let history = LocalHistory::open(&root.join("h"), Limits::default()).unwrap();
    let file = root.join("p").join("A.txt");
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, b"old version").unwrap();
    history.record_file(&file, Cause::Saved).unwrap();
    history.record_removed(&root.join("p")).unwrap();
    fs::remove_file(&file).unwrap();
    // the window lists the deleted file...
    let deleted = history.deleted_under(&root.join("p"));
    assert_eq!(deleted.len(), 1);
    // ...but before the user presses Restore somebody creates a new file at that path
    fs::write(&file, b"NEW WORK, never saved to history").unwrap();
    history.restore(&deleted[0]).unwrap();
    let on_disk = fs::read(&file).unwrap();
    let kept = history.file_history(&file).iter().any(|r| {
        history
            .content(r)
            .map(|b| b == b"NEW WORK, never saved to history")
            .unwrap_or(false)
    });
    eprintln!(
        "on disk now: {:?}; the overwritten text is in the history: {kept}",
        String::from_utf8_lossy(&on_disk)
    );
    assert_eq!(on_disk, b"old version");
    assert!(kept, "restore destroyed the newer file and kept no copy");
}

#[test]
fn one_big_version_does_not_wipe_the_other_files_history() {
    let root = temp("prune");
    let limits = Limits {
        max_age: Duration::from_secs(3600),
        max_bytes: 1000,
        ..Limits::default()
    };
    let history = LocalHistory::open(&root.join("h"), limits).unwrap();
    let small = root.join("small.txt");
    fs::write(&small, b"small file with history").unwrap();
    history.record_file(&small, Cause::Saved).unwrap();
    let big = root.join("big.bin");
    // incompressible bytes bigger than the limit
    let bytes: Vec<u8> = (0..20_000u32)
        .map(|i| (i.wrapping_mul(2654435761) >> 13) as u8)
        .collect();
    fs::write(&big, &bytes).unwrap();
    history.record_file(&big, Cause::Saved).unwrap();
    let small_left = history.file_history(&small).len();
    let big_left = history.file_history(&big).len();
    eprintln!(
        "after one oversized version: small file revisions {small_left}, big file revisions {big_left}"
    );
    assert_eq!(small_left, 1, "the history of an unrelated file was wiped");
}

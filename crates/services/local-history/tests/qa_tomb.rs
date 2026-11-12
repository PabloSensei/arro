use std::fs;
use std::time::Duration;

use local_history::{Cause, Limits, LocalHistory};

fn noise(seed: u32, len: usize) -> Vec<u8> {
    (0..len as u32)
        .map(|i| ((i ^ seed).wrapping_mul(2654435761) >> 13) as u8)
        .collect()
}

#[test]
fn a_deleted_file_can_still_be_restored_when_the_size_limit_bites() {
    let root = test_support::scratch("qa-tomb");
    fs::create_dir_all(root.join("p")).unwrap();
    let limits = Limits {
        max_age: Duration::from_secs(3600),
        max_bytes: 3000,
        ..Limits::default()
    };
    let history = LocalHistory::open(&root.join("h"), limits).unwrap();
    let a = root.join("p").join("A.bin");
    fs::write(&a, noise(1, 300)).unwrap();
    history.record_file(&a, Cause::Saved).unwrap();
    // delete as the project panel does: snapshot of the tree, then the tombstone
    history
        .record_tree(&root.join("p"), Cause::BeforeDelete)
        .ok();
    history.record_removed(&root.join("p")).unwrap();
    fs::remove_file(&a).unwrap();
    assert_eq!(
        history.deleted_under(&root.join("p")).len(),
        1,
        "before pressure"
    );
    let b = root.join("q").join("B.bin");
    fs::create_dir_all(b.parent().unwrap()).unwrap();
    for round in 0..8u32 {
        fs::write(&b, noise(100 + round, 900)).unwrap();
        history.record_file(&b, Cause::Saved).unwrap();
    }
    history.prune().unwrap();
    let left = history.deleted_under(&root.join("p")).len();
    let b_left = history.file_history(&b).len();
    eprintln!("deleted files still restorable: {left}; B revisions: {b_left}");
    assert_eq!(left, 1, "the deleted file fell out of the history");
}

use std::sync::atomic::{AtomicU32, Ordering};

use super::*;

/// A fresh folder with a project and a history inside, gone after the test.
struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let root = std::env::temp_dir().join(format!(
            "local-history-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("project/src")).unwrap();
        Self { root }
    }

    fn project(&self, relative: &str) -> PathBuf {
        self.root.join("project").join(relative)
    }

    fn history(&self, limits: Limits) -> LocalHistory {
        LocalHistory::open(&self.root.join("history"), limits).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn a_save_keeps_the_exact_bytes_and_survives_a_reopen() {
    let fixture = Fixture::new();
    let file = fixture.project("src/Owner.txt");
    // UTF-16 with a BOM and CRLF line ends: the bytes come back as they were.
    let mut bytes = vec![0xFF, 0xFE];
    for unit in "one\r\nдва\r\n".encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    fs::write(&file, &bytes).unwrap();
    {
        let history = fixture.history(Limits::default());
        let saved = history.record_file(&file, Cause::Saved).unwrap().unwrap();
        assert_eq!(history.content(&saved).unwrap(), bytes);
        // The same bytes again add nothing.
        assert!(history.record_file(&file, Cause::Saved).unwrap().is_none());
        fs::write(&file, b"three\n").unwrap();
        history.record_file(&file, Cause::Saved).unwrap().unwrap();
    }

    let history = fixture.history(Limits::default());
    let versions = history.file_history(&file);
    assert_eq!(versions.len(), 2);
    assert_eq!(history.content(&versions[0]).unwrap(), b"three\n");
    assert_eq!(history.content(&versions[1]).unwrap(), bytes);
    let other_case = PathBuf::from(file.to_string_lossy().to_uppercase());
    assert_eq!(history.file_history(&other_case).len(), 2);
}

#[test]
fn the_size_limit_drops_the_oldest_versions_and_their_contents() {
    let fixture = Fixture::new();
    let file = fixture.project("src/Big.txt");
    let limits = Limits {
        max_age: Duration::from_secs(3600),
        max_bytes: 3000,
        ..Limits::default()
    };
    let history = fixture.history(limits);
    for round in 0..6u32 {
        // Hardly compressible, about 1000 bytes each.
        let mut seed = 0x9E37_79B9u32 ^ (round + 1);
        let bytes: Vec<u8> = (0..1000)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                seed as u8
            })
            .collect();
        history.record(&file, Some(&bytes), Cause::Saved).unwrap();
    }

    let versions = history.file_history(&file);
    assert!(
        versions.len() < 6 && !versions.is_empty(),
        "{}",
        versions.len()
    );
    assert!(history.blobs.total().unwrap() <= 3000);
    let stored = fs::read_dir(history.blobs.dir()).unwrap().count();
    assert_eq!(stored, versions.len());
}

#[test]
fn the_age_limit_drops_old_versions_at_a_prune() {
    let fixture = Fixture::new();
    let file = fixture.project("src/Old.txt");
    let history = fixture.history(Limits::default());
    let long_ago = SystemTime::now() - Duration::from_secs(30 * 24 * 60 * 60);
    history
        .record_at(&file, Some(b"old"), Cause::Saved, long_ago)
        .unwrap();
    history.record(&file, Some(b"new"), Cause::Saved).unwrap();

    history.prune().unwrap();

    let versions = history.file_history(&file);
    assert_eq!(versions.len(), 1);
    assert_eq!(history.content(&versions[0]).unwrap(), b"new");
    drop(history);
    assert_eq!(
        fixture.history(Limits::default()).file_history(&file).len(),
        1
    );
}

#[test]
fn a_deleted_file_is_found_in_its_folder_and_restored() {
    let fixture = Fixture::new();
    let file = fixture.project("src/Gone.txt");
    let kept = fixture.project("src/Kept.txt");
    fs::write(&file, b"was here\r\n").unwrap();
    fs::write(&kept, b"stays").unwrap();
    let history = fixture.history(Limits::default());
    history
        .record_tree(&fixture.project("src"), Cause::BeforeDelete)
        .unwrap();
    fs::remove_file(&file).unwrap();
    history.record_removed(&file).unwrap();

    let deleted = history.deleted_under(&fixture.project(""));
    assert_eq!(deleted.len(), 1);
    assert_eq!(deleted[0].path, file);
    history.restore(&deleted[0]).unwrap();

    assert_eq!(fs::read(&file).unwrap(), b"was here\r\n");
    assert!(history.deleted_under(&fixture.project("")).is_empty());
    let marks: Vec<_> = history
        .file_history(&file)
        .iter()
        .map(|it| it.content.is_some())
        .collect();
    assert_eq!(marks, [false, true]);
}

#[test]
fn a_label_shows_up_in_the_history_of_every_file_under_it() {
    let fixture = Fixture::new();
    let file = fixture.project("src/Owner.txt");
    let history = fixture.history(Limits::default());
    history.record(&file, Some(b"a"), Cause::Saved).unwrap();
    history
        .put_label(&fixture.project(""), "Before running\ttests")
        .unwrap();
    history
        .put_label(&fixture.root.join("elsewhere"), "Not here")
        .unwrap();

    let versions = history.file_history(&file);
    assert_eq!(versions.len(), 2);
    assert!(versions[0].is_label());
    assert_eq!(versions[0].label.as_deref(), Some("Before running tests"));
    assert_eq!(history.folder_history(&fixture.project("src")).len(), 2);
}

#[test]
fn a_restore_refuses_to_overwrite_a_file_too_big_to_keep() {
    let fixture = Fixture::new();
    let file = fixture.project("src/A.txt");
    let limits = Limits {
        max_file_bytes: 16,
        ..Limits::default()
    };
    let history = fixture.history(limits);
    fs::write(&file, b"small").unwrap();
    let saved = history.record_file(&file, Cause::Saved).unwrap().unwrap();
    fs::write(&file, b"a file much bigger than the cap").unwrap();

    assert!(history.restore(&saved).is_err());
    assert_eq!(fs::read(&file).unwrap(), b"a file much bigger than the cap");
}

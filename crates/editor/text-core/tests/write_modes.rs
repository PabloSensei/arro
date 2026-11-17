//! Saving replaces the file but keeps what the file was: its mode, and the refusal of a
//! target that may not be written.

#[cfg(unix)]
use platform::{mode, set_mode};
#[cfg(unix)]
use text_core::{read_file, write_file};

/// A read-only target is refused on Unix too, where the rename would replace it since
/// only the folder needs the write permission.
#[cfg(unix)]
#[test]
fn a_read_only_file_refuses_the_save_and_keeps_its_bytes() {
    let directory = test_support::scratch("text-core-read-only");
    let path = directory.join("locked.txt");
    std::fs::write(&path, b"one\n").unwrap();
    let format = read_file(&path).unwrap().format;
    set_mode(&path, 0o444).unwrap();

    let error = write_file(&path, "two\n", &format).unwrap_err();

    assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
    assert!(error.to_string().contains("read-only"), "{error}");
    assert_eq!(std::fs::read(&path).unwrap(), b"one\n");
    assert_eq!(
        std::fs::read_dir(&directory).unwrap().count(),
        1,
        "no temporary of the failed save is left"
    );
}

/// A file that may be written keeps its mode, the executable bit included.
#[cfg(unix)]
#[test]
fn a_saved_file_keeps_its_mode() {
    let directory = test_support::scratch("text-core-mode");
    let path = directory.join("run.sh");
    std::fs::write(&path, b"#!/bin/sh\necho one\n").unwrap();
    set_mode(&path, 0o755).unwrap();
    let format = read_file(&path).unwrap().format;

    write_file(&path, "#!/bin/sh\necho two\n", &format).unwrap();

    assert_eq!(mode(&path).unwrap(), 0o755, "the mode of the file stayed");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "#!/bin/sh\necho two\n"
    );
}

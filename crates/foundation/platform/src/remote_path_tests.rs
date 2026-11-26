//! Paths that lead to another machine, which the IDE never reads or starts.

use std::path::Path;

#[test]
fn a_share_of_another_machine_is_remote() {
    assert!(crate::is_remote_path(Path::new(r"\\server\share\jdk")));
    assert!(crate::is_remote_path(Path::new("//server/share/jdk")));
}

#[test]
fn a_plain_folder_is_not_a_share() {
    assert!(!crate::is_remote_path(Path::new("relative/folder")));
    #[cfg(unix)]
    assert!(!crate::is_remote_path(Path::new("/usr/lib/jvm")));
    #[cfg(windows)]
    {
        let folder = test_support::TempDir::new("platform-remote-local");
        assert!(!crate::is_remote_path(folder.path()));
    }
}

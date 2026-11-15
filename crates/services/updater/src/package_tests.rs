//! An installation from a `.deb`, a `.rpm` or another package owns its folder: the
//! updater must not rename or overwrite files there, and points at the release page
//! instead.

use super::*;

#[test]
fn the_folders_of_the_package_manager_are_installations_from_a_package() {
    assert!(installed_from_package(Path::new("/opt/arro")));
    assert!(installed_from_package(Path::new("/usr/lib/arro")));
    // A program unpacked into the home folder stays the user's own.
    assert!(!installed_from_package(Path::new("/home/user/opt/arro")));
}

/// The `.rpm` carries the same tree as the `.deb`: the program in `/opt/arro` and a link
/// in `/usr/bin`, so the check that keeps the updater out of a package installation is
/// the one both formats need.
#[test]
fn an_rpm_installation_is_a_package_installation() {
    assert!(installed_from_package(Path::new("/opt/arro")));
    assert!(installed_from_package(Path::new("/usr/lib64/arro")));
    // `rpm --relocate` into the home folder makes the folder the user's own again.
    assert!(!installed_from_package(Path::new(
        "/home/user/.local/share/arro"
    )));
}

#[cfg(windows)]
#[test]
fn program_files_is_an_installation_from_a_package() {
    assert!(installed_from_package(Path::new(r"C:\Program Files\Arro")));
    assert!(installed_from_package(Path::new(
        r"C:\Program Files (x86)\Arro"
    )));
    assert!(!installed_from_package(Path::new(
        r"C:\Users\user\Downloads\Arro"
    )));
}

#[test]
fn a_folder_of_the_user_that_can_be_written_is_an_ordinary_installation() {
    let folder = test_support::scratch("package-user");
    assert!(!installed_from_package(&folder));
}

#[cfg(unix)]
#[test]
fn a_folder_without_write_permission_is_an_installation_from_a_package() {
    let folder = test_support::scratch("package-readonly").join("arro");
    fs::create_dir_all(folder.join("tools")).unwrap();
    fs::write(folder.join("arro"), "program").unwrap();
    platform::set_mode(&folder, 0o555).unwrap();

    assert!(installed_from_package(&folder));

    // The test folder is removed with the thread; the folder has to be writable again
    // by then.
    platform::set_mode(&folder, 0o755).unwrap();
}

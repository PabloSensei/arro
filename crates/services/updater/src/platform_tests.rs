//! The systems the project publishes a release for and the systems it does not: the
//! archive is chosen only where a release exists, and a check there never asks the
//! server.

use super::*;

fn version(text: &str) -> Version {
    Version::parse(text).unwrap()
}

#[test]
fn a_system_without_a_release_has_no_archive_to_update_to() {
    assert_eq!(
        archive_name_for(&version("0.2.0"), Some(Platform::WindowsX86_64)),
        Some("arro-0.2.0-windows-x86_64.zip".to_owned())
    );
    assert_eq!(archive_name_for(&version("0.2.0"), None), None);
}

#[test]
fn a_manual_check_at_a_system_without_a_release_tells_the_user_to_build_from_source() {
    let notice = manual_check_notice(None).expect("a notice for a system without a release");
    assert_eq!(notice, NO_RELEASE_NOTICE);
    assert!(notice.contains("not available"), "{notice}");
    assert!(notice.contains("from source"), "{notice}");
}

#[test]
fn a_manual_check_at_a_system_with_a_release_asks_the_server() {
    for platform in Platform::ALL {
        assert_eq!(
            manual_check_notice(Some(platform)),
            None,
            "{}",
            platform.name()
        );
    }
}

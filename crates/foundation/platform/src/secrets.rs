//! Passwords kept by the system: the Credential Manager of Windows, the Keychain of
//! macOS and the Secret Service of Linux.
//!
//! A secret enters this module only as [`Secret`] and leaves it only as the same type:
//! neither [`std::fmt::Debug`] nor an error text ever shows its value, and the bytes are
//! overwritten when the value goes. Every call blocks on the system, so a caller on the
//! interface starts it in the background.

use std::fmt;

#[cfg(windows)]
#[path = "secrets_windows.rs"]
mod imp;
#[cfg(target_os = "macos")]
#[path = "secrets_macos.rs"]
mod imp;
#[cfg(all(unix, not(target_os = "macos")))]
#[path = "secrets_unix.rs"]
mod imp;

/// The product part of every service name, which keeps the entries of the IDE apart from
/// the entries of other programs in the one store of the system.
pub const APPLICATION: &str = "Arro";

/// The name of a secret: `<application>/<group>/<id>`, as every platform stores it.
pub fn service_name(application: &str, group: &str, id: &str) -> String {
    format!("{application}/{group}/{id}")
}

/// A password in memory. It carries no formatting and no serialization, so no log line,
/// no `Debug` of a structure and no error text can show it.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Secret(<redacted>)")
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Secret(<redacted>)")
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        // A best effort overwrite of the bytes this value owned: another allocation may
        // already hold a copy, but the common case leaves no plain password behind.
        unsafe {
            for byte in self.0.as_bytes_mut() {
                std::ptr::write_volatile(byte, 0);
            }
        }
    }
}

/// Whether the system of this machine has a store of passwords. The reason of a failure
/// names what to install and never what a stored secret is.
pub fn available() -> Result<(), String> {
    imp::available()
}

/// Writes the secret of `account` under `service`, replacing the one that was there.
pub fn store(service: &str, account: &str, secret: &Secret) -> Result<(), String> {
    check_key(service, account)?;
    imp::store(service, account, secret)
}

/// Reads the secret of `account` under `service`, or nothing when there is none.
pub fn load(service: &str, account: &str) -> Option<Secret> {
    if check_key(service, account).is_err() {
        return None;
    }
    imp::load(service, account)
}

/// Removes the secret of `account` under `service`; a missing entry is not an error.
pub fn delete(service: &str, account: &str) -> Result<(), String> {
    check_key(service, account)?;
    imp::delete(service, account)
}

/// The service and the account are names a system writes into its store, not free text:
/// an empty name or a byte that ends a string early cannot address an entry.
fn check_key(service: &str, account: &str) -> Result<(), String> {
    if service.is_empty() {
        return Err("The name of the password service is empty.".to_owned());
    }
    if account.is_empty() {
        return Err("The account of the password is empty.".to_owned());
    }
    if service.contains('\0') || account.contains('\0') {
        return Err("The name of a password entry has a zero byte.".to_owned());
    }
    Ok(())
}

/// The text of a secret as a platform gives it back. The bytes of a binary value are not
/// a password, so a value that is not UTF-8 is no secret at all.
fn secret_from_bytes(bytes: &[u8]) -> Option<Secret> {
    std::str::from_utf8(bytes).ok().map(Secret::new)
}

/// The secret in the answer of `secret-tool lookup`, which ends its line with a newline.
/// Exactly one line end is the one the program adds; the password itself may not end
/// with a newline, because the store cannot tell the two apart.
#[cfg(any(target_os = "linux", test))]
fn secret_from_lookup(answer: &[u8]) -> Option<Secret> {
    let answer = answer
        .strip_suffix(b"\r\n")
        .or_else(|| answer.strip_suffix(b"\n"))
        .unwrap_or(answer);
    secret_from_bytes(answer)
}

/// The codes of `SecItem.h` and what a caller does with each. The codes live here rather
/// than beside the calls of macOS, so the mapping is tested on every platform.
#[cfg(any(target_os = "macos", test))]
pub(crate) mod sec_status {
    /// `errSecSuccess`.
    pub const ERR_SEC_SUCCESS: i32 = 0;
    /// `errSecDuplicateItem`: the entry exists, so an update replaces it.
    pub const ERR_SEC_DUPLICATE_ITEM: i32 = -25299;
    /// `errSecItemNotFound`: there is no entry, which `load` reports as no password and
    /// `delete` as nothing to do.
    pub const ERR_SEC_ITEM_NOT_FOUND: i32 = -25300;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Outcome {
        Success,
        Missing,
        Duplicate,
        Other(i32),
    }

    pub fn outcome(status: i32) -> Outcome {
        match status {
            ERR_SEC_SUCCESS => Outcome::Success,
            ERR_SEC_ITEM_NOT_FOUND => Outcome::Missing,
            ERR_SEC_DUPLICATE_ITEM => Outcome::Duplicate,
            other => Outcome::Other(other),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sec_status::{
        ERR_SEC_DUPLICATE_ITEM, ERR_SEC_ITEM_NOT_FOUND, ERR_SEC_SUCCESS, Outcome, outcome,
    };

    #[test]
    fn a_service_names_the_product_its_group_and_its_source() {
        assert_eq!(
            service_name(APPLICATION, "database", "owners"),
            "Arro/database/owners"
        );
        assert_eq!(service_name("Other", "group", "id"), "Other/group/id");
    }

    #[test]
    fn a_secret_shows_itself_nowhere() {
        let secret = Secret::new("hunter2");
        assert!(!format!("{secret:?}").contains("hunter2"));
        assert!(!format!("{secret}").contains("hunter2"));
        assert_eq!(format!("{secret:?}"), "Secret(<redacted>)");
        assert_eq!(secret.expose(), "hunter2");
    }

    #[test]
    fn a_bad_key_fails_before_the_system_is_asked_and_carries_no_secret() {
        let secret = Secret::new("hunter2");
        for error in [
            store("", "sa", &secret).unwrap_err(),
            store("Arro/database/owners", "", &secret).unwrap_err(),
            delete("Arro/database/owners\0x", "sa").unwrap_err(),
        ] {
            assert!(!error.contains("hunter2"), "{error}");
        }
        assert!(load("", "sa").is_none());
    }

    #[test]
    fn the_answer_of_lookup_loses_only_its_line_end() {
        assert_eq!(
            secret_from_lookup(b"hunter2\n").map(|secret| secret.expose().to_owned()),
            Some("hunter2".to_owned())
        );
        assert_eq!(
            secret_from_lookup(b"hunter2\r\n").map(|secret| secret.expose().to_owned()),
            Some("hunter2".to_owned())
        );
        assert_eq!(
            secret_from_lookup(b" hunter2 ").map(|secret| secret.expose().to_owned()),
            Some(" hunter2 ".to_owned())
        );
        assert_eq!(
            secret_from_lookup(b"two\nlines\n").map(|secret| secret.expose().to_owned()),
            Some("two\nlines".to_owned())
        );
        assert!(secret_from_lookup(b"\xff\n").is_none());
    }

    #[test]
    fn the_bytes_of_a_platform_answer_are_read_as_utf8() {
        assert_eq!(
            secret_from_bytes("пароль".as_bytes()).map(|secret| secret.expose().to_owned()),
            Some("пароль".to_owned())
        );
        assert!(secret_from_bytes(b"\xff\xfe").is_none());
    }

    #[test]
    fn the_security_framework_codes_are_told_apart() {
        assert_eq!(outcome(ERR_SEC_SUCCESS), Outcome::Success);
        assert_eq!(outcome(ERR_SEC_ITEM_NOT_FOUND), Outcome::Missing);
        assert_eq!(outcome(ERR_SEC_DUPLICATE_ITEM), Outcome::Duplicate);
        assert_eq!(outcome(-25308), Outcome::Other(-25308));
    }
}

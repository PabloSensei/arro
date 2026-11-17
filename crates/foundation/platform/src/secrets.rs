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

/// What a store answered to a call that removes an entry, in the words of its platform:
/// the exit code and the error stream of a program, or the status of the
/// Security.framework. The `security` program of macOS reports a missing item with the
/// exit code 44, but the IDE talks to the Keychain directly, so its answer is a status
/// rather than a code.
#[cfg(any(unix, test))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DeleteAnswer {
    /// The exit code of a store program and what it wrote to its error stream.
    #[cfg(any(all(unix, not(target_os = "macos")), test))]
    Program { code: Option<i32>, stderr: String },
    /// The `OSStatus` of `SecItemDelete`.
    #[cfg(any(target_os = "macos", test))]
    Security(i32),
}

/// Whether a delete is done: the call succeeded, or the entry was already absent, which
/// [`delete`] promises is not an error. Every other answer stays a failure with the text
/// of the platform, so a locked store or a session without D-Bus is still reported.
///
/// The rule lives here rather than beside each platform, so the workspace tests it
/// everywhere and each platform only supplies the numbers and the words it gets back.
#[cfg(any(unix, test))]
pub(crate) fn delete_is_done(answer: DeleteAnswer) -> bool {
    match answer {
        #[cfg(any(all(unix, not(target_os = "macos")), test))]
        DeleteAnswer::Program { code, stderr } => {
            code == Some(0) || program_says_absent(code, &stderr)
        }
        #[cfg(any(target_os = "macos", test))]
        DeleteAnswer::Security(status) => matches!(
            sec_status::outcome(status),
            sec_status::Outcome::Success | sec_status::Outcome::Missing
        ),
    }
}

/// The words with which a store program says that there is no entry to remove: the
/// message of libsecret for an item that has gone, the message of D-Bus for a path that
/// the service no longer has (the path of an item or a collection, never the root object
/// of the service, whose absence is a real failure), and the message of `security` for an
/// item that the Keychain does not hold. Every other text stays a failure: a locked store
/// and a session without D-Bus have their own words.
#[cfg(any(all(unix, not(target_os = "macos")), test))]
const ABSENT_TEXTS: [&str; 4] = [
    "no such secret item",
    "secret item was not found",
    "no such object at path /org/freedesktop/secrets/",
    "could not be found in the keychain",
];

/// Whether a failed call of a store program means that there was no entry to remove. The
/// exit code alone cannot say it: `secret-tool` ends with 1 for every failure, so the
/// words of its error stream are the sign, and only the known words of a missing entry
/// count. The call asks the tool for the untranslated text of the C locale, so the words
/// are the same on every machine.
#[cfg(any(all(unix, not(target_os = "macos")), test))]
fn program_says_absent(code: Option<i32>, stderr: &str) -> bool {
    /// The only failure code of `secret-tool`.
    const FAILURE: i32 = 1;
    if code != Some(FAILURE) {
        return false;
    }
    let stderr = stderr.to_ascii_lowercase();
    ABSENT_TEXTS.iter().any(|text| stderr.contains(text))
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

    #[test]
    fn a_delete_is_done_when_the_entry_is_gone_or_was_absent() {
        assert!(delete_is_done(DeleteAnswer::Program {
            code: Some(0),
            stderr: String::new(),
        }));
        assert!(delete_is_done(DeleteAnswer::Program {
            code: Some(1),
            stderr:
                "clear: No such secret item at path /org/freedesktop/secrets/collection/login/7\n"
                    .to_owned(),
        }));
        assert!(delete_is_done(DeleteAnswer::Program {
            code: Some(1),
            stderr: "clear: The secret item was not found\n".to_owned(),
        }));
        assert!(delete_is_done(DeleteAnswer::Program {
            code: Some(1),
            stderr: "clear: No such object at path /org/freedesktop/secrets/collection/login/7\n"
                .to_owned(),
        }));
        assert!(delete_is_done(DeleteAnswer::Security(ERR_SEC_SUCCESS)));
        assert!(delete_is_done(DeleteAnswer::Security(
            ERR_SEC_ITEM_NOT_FOUND
        )));
    }

    #[test]
    fn a_delete_that_failed_for_another_reason_is_not_done() {
        for (code, stderr) in [
            (Some(1), ""),
            (
                Some(1),
                "clear: Cannot autolaunch D-Bus without X11 $DISPLAY",
            ),
            (Some(1), "clear: The secret service was not available"),
            (
                Some(1),
                "clear: No such object at path /org/freedesktop/secrets",
            ),
            (
                Some(2),
                "clear: No such secret item at path /org/freedesktop/secrets/collection/login/7",
            ),
            (
                None,
                "clear: No such secret item at path /org/freedesktop/secrets/collection/login/7",
            ),
        ] {
            assert!(
                !delete_is_done(DeleteAnswer::Program {
                    code,
                    stderr: stderr.to_owned(),
                }),
                "code {code:?} with {stderr:?}"
            );
        }
        assert!(!delete_is_done(DeleteAnswer::Security(-25308)));
    }
}

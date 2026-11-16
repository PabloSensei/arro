//! The Credential Manager of Windows: one generic credential per service and account,
//! kept in the store of the local machine.

use std::ffi::c_void;
use std::ptr::null_mut;
use std::slice;

use windows_sys::Win32::Foundation::{ERROR_NOT_FOUND, GetLastError};
use windows_sys::Win32::Security::Credentials::{
    CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC, CREDENTIALW, CredDeleteW, CredFree, CredReadW,
    CredWriteW,
};

use super::{Secret, secret_from_bytes};

/// The target name of one entry. The account is a part of it, so two users of one
/// database get two entries rather than one.
fn target(service: &str, account: &str) -> String {
    format!("{service}/{account}")
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn failure(action: &str, code: u32) -> String {
    format!("The Credential Manager could not {action} the password (Windows error {code}).")
}

pub fn available() -> Result<(), String> {
    Ok(())
}

pub fn store(service: &str, account: &str, secret: &Secret) -> Result<(), String> {
    let target = wide(&target(service, account));
    let user = wide(account);
    let comment = wide("Arro");
    let mut blob = secret.expose().as_bytes().to_vec();
    let credential = CREDENTIALW {
        Type: CRED_TYPE_GENERIC,
        TargetName: target.as_ptr() as *mut u16,
        Comment: comment.as_ptr() as *mut u16,
        CredentialBlobSize: blob.len() as u32,
        CredentialBlob: blob.as_mut_ptr(),
        Persist: CRED_PERSIST_LOCAL_MACHINE,
        UserName: user.as_ptr() as *mut u16,
        ..Default::default()
    };
    // The call copies the blob and the strings before it returns, so the buffers stay
    // alive for the whole call and may go together with this frame.
    let written = unsafe { CredWriteW(&credential, 0) };
    if written == 0 {
        return Err(failure("save", unsafe { GetLastError() }));
    }
    Ok(())
}

pub fn load(service: &str, account: &str) -> Option<Secret> {
    let target = wide(&target(service, account));
    let mut pointer: *mut CREDENTIALW = null_mut();
    if unsafe { CredReadW(target.as_ptr(), CRED_TYPE_GENERIC, 0, &mut pointer) } == 0 {
        // A missing entry is no password; every other failure leaves the caller to ask
        // the user, which is safe and does not show a secret.
        return None;
    }
    if pointer.is_null() {
        return None;
    }
    let credential = unsafe { &*pointer };
    let bytes = if credential.CredentialBlobSize == 0 || credential.CredentialBlob.is_null() {
        &[][..]
    } else {
        unsafe {
            slice::from_raw_parts(
                credential.CredentialBlob,
                credential.CredentialBlobSize as usize,
            )
        }
    };
    let secret = secret_from_bytes(bytes);
    // The copy is ours now; the buffer of the Credential Manager goes with `CredFree`.
    unsafe { CredFree(pointer as *const c_void) };
    secret
}

pub fn delete(service: &str, account: &str) -> Result<(), String> {
    let target = wide(&target(service, account));
    if unsafe { CredDeleteW(target.as_ptr(), CRED_TYPE_GENERIC, 0) } != 0 {
        return Ok(());
    }
    let code = unsafe { GetLastError() };
    if code == ERROR_NOT_FOUND {
        return Ok(());
    }
    Err(failure("delete", code))
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    /// The service of one test, which no other run on the machine shares.
    fn unique_service() -> String {
        static SERIAL: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos());
        format!(
            "Arro/test/{}-{nanos}-{}",
            std::process::id(),
            SERIAL.fetch_add(1, Ordering::Relaxed)
        )
    }

    /// The entry of a test goes even when an assertion fails before the delete.
    struct Cleanup {
        service: String,
        account: String,
    }

    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = delete(&self.service, &self.account);
        }
    }

    #[test]
    fn a_secret_is_stored_read_and_deleted() {
        let service = unique_service();
        let account = "sa".to_owned();
        let _cleanup = Cleanup {
            service: service.clone(),
            account: account.clone(),
        };
        let secret = Secret::new("hunter2");
        available().unwrap();

        store(&service, &account, &secret).unwrap();
        assert_eq!(
            load(&service, &account).as_ref().map(Secret::expose),
            Some("hunter2")
        );

        delete(&service, &account).unwrap();
        assert!(load(&service, &account).is_none());
        // A second delete of a missing entry is not an error.
        delete(&service, &account).unwrap();
    }

    #[test]
    fn a_second_store_replaces_the_secret() {
        let service = unique_service();
        let account = "sa".to_owned();
        let _cleanup = Cleanup {
            service: service.clone(),
            account: account.clone(),
        };

        store(&service, &account, &Secret::new("first")).unwrap();
        store(&service, &account, &Secret::new("second")).unwrap();
        assert_eq!(
            load(&service, &account).as_ref().map(Secret::expose),
            Some("second")
        );
    }
}

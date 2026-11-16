//! The Keychain of macOS, through the generic-password items of Security.framework.
//! CoreFoundation builds the query dictionaries and owns every value in them, so the
//! strings and the data are released as soon as the item call answers.

use std::ffi::c_void;
use std::ptr::{addr_of, null, null_mut};
use std::slice;

use super::sec_status::{self, Outcome};
use super::{Secret, secret_from_bytes};

type CFTypeRef = *const c_void;
type CFStringRef = *const c_void;
type CFDataRef = *const c_void;
type CFDictionaryRef = *const c_void;
type CFIndex = isize;
type Boolean = u8;
type OSStatus = i32;

/// `kCFStringEncodingUTF8` of `CFStringEncoding.h`.
const CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFDictionaryCreate(
        allocator: CFTypeRef,
        keys: *const CFTypeRef,
        values: *const CFTypeRef,
        count: CFIndex,
        key_callbacks: *const c_void,
        value_callbacks: *const c_void,
    ) -> CFDictionaryRef;
    fn CFStringCreateWithBytes(
        allocator: CFTypeRef,
        bytes: *const u8,
        length: CFIndex,
        encoding: u32,
        external: Boolean,
    ) -> CFStringRef;
    fn CFDataCreate(allocator: CFTypeRef, bytes: *const u8, length: CFIndex) -> CFDataRef;
    fn CFDataGetBytePtr(data: CFDataRef) -> *const u8;
    fn CFDataGetLength(data: CFDataRef) -> CFIndex;
    fn CFRelease(value: CFTypeRef);
    static kCFBooleanTrue: CFTypeRef;
    static kCFTypeDictionaryKeyCallBacks: c_void;
    static kCFTypeDictionaryValueCallBacks: c_void;
}

#[link(name = "Security", kind = "framework")]
unsafe extern "C" {
    fn SecItemAdd(attributes: CFDictionaryRef, result: *mut CFTypeRef) -> OSStatus;
    fn SecItemCopyMatching(query: CFDictionaryRef, result: *mut CFTypeRef) -> OSStatus;
    fn SecItemUpdate(query: CFDictionaryRef, attributes: CFDictionaryRef) -> OSStatus;
    fn SecItemDelete(query: CFDictionaryRef) -> OSStatus;
    static kSecClass: CFStringRef;
    static kSecClassGenericPassword: CFStringRef;
    static kSecAttrService: CFStringRef;
    static kSecAttrAccount: CFStringRef;
    static kSecValueData: CFStringRef;
    static kSecReturnData: CFStringRef;
    static kSecMatchLimit: CFStringRef;
    static kSecMatchLimitOne: CFStringRef;
}

/// A CoreFoundation value this module created, released when it goes.
struct Owned(CFTypeRef);

impl Drop for Owned {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CFRelease(self.0) };
        }
    }
}

fn string(text: &str) -> Option<Owned> {
    let pointer = unsafe {
        CFStringCreateWithBytes(
            null(),
            text.as_ptr(),
            text.len() as CFIndex,
            CF_STRING_ENCODING_UTF8,
            0,
        )
    };
    (!pointer.is_null()).then_some(Owned(pointer))
}

fn data(bytes: &[u8]) -> Option<Owned> {
    let pointer = unsafe { CFDataCreate(null(), bytes.as_ptr(), bytes.len() as CFIndex) };
    (!pointer.is_null()).then_some(Owned(pointer))
}

fn dictionary(entries: &[(CFTypeRef, CFTypeRef)]) -> Option<Owned> {
    let keys: Vec<CFTypeRef> = entries.iter().map(|(key, _)| *key).collect();
    let values: Vec<CFTypeRef> = entries.iter().map(|(_, value)| *value).collect();
    let key_callbacks = addr_of!(kCFTypeDictionaryKeyCallBacks).cast();
    let value_callbacks = addr_of!(kCFTypeDictionaryValueCallBacks).cast();
    let pointer = unsafe {
        CFDictionaryCreate(
            null(),
            keys.as_ptr(),
            values.as_ptr(),
            entries.len() as CFIndex,
            key_callbacks,
            value_callbacks,
        )
    };
    (!pointer.is_null()).then_some(Owned(pointer))
}

fn failure(action: &str, status: OSStatus) -> String {
    format!("The Keychain could not {action} the password (OSStatus {status}).")
}

/// The class, the service and the account of one generic-password entry.
fn key(service: CFTypeRef, account: CFTypeRef) -> Option<Owned> {
    let class = unsafe { kSecClass };
    let generic = unsafe { kSecClassGenericPassword };
    let service_attr = unsafe { kSecAttrService };
    let account_attr = unsafe { kSecAttrAccount };
    dictionary(&[
        (class, generic),
        (service_attr, service),
        (account_attr, account),
    ])
}

pub fn available() -> Result<(), String> {
    Ok(())
}

pub fn store(service: &str, account: &str, secret: &Secret) -> Result<(), String> {
    let service = string(service).ok_or_else(|| failure("save", -1))?;
    let account = string(account).ok_or_else(|| failure("save", -1))?;
    let value = data(secret.expose().as_bytes()).ok_or_else(|| failure("save", -1))?;
    let key = key(service.0, account.0).ok_or_else(|| failure("save", -1))?;
    let value_data = unsafe { kSecValueData };
    let attributes = dictionary(&[(value_data, value.0)]).ok_or_else(|| failure("save", -1))?;
    let add = dictionary(&[
        (unsafe { kSecClass }, unsafe { kSecClassGenericPassword }),
        (unsafe { kSecAttrService }, service.0),
        (unsafe { kSecAttrAccount }, account.0),
        (value_data, value.0),
    ])
    .ok_or_else(|| failure("save", -1))?;
    let status = unsafe { SecItemAdd(add.0, null_mut()) };
    match sec_status::outcome(status) {
        Outcome::Success => Ok(()),
        Outcome::Duplicate => {
            let status = unsafe { SecItemUpdate(key.0, attributes.0) };
            match sec_status::outcome(status) {
                Outcome::Success => Ok(()),
                _ => Err(failure("save", status)),
            }
        }
        _ => Err(failure("save", status)),
    }
}

pub fn load(service: &str, account: &str) -> Option<Secret> {
    let service = string(service)?;
    let account = string(account)?;
    let query = dictionary(&[
        (unsafe { kSecClass }, unsafe { kSecClassGenericPassword }),
        (unsafe { kSecAttrService }, service.0),
        (unsafe { kSecAttrAccount }, account.0),
        (unsafe { kSecReturnData }, unsafe { kCFBooleanTrue }),
        (unsafe { kSecMatchLimit }, unsafe { kSecMatchLimitOne }),
    ])?;
    let mut result: CFTypeRef = null();
    let status = unsafe { SecItemCopyMatching(query.0, &mut result) };
    if !matches!(sec_status::outcome(status), Outcome::Success) || result.is_null() {
        return None;
    }
    let data = Owned(result);
    let length = unsafe { CFDataGetLength(data.0) };
    if length < 0 {
        return None;
    }
    if length == 0 {
        return Some(Secret::new(""));
    }
    let bytes = unsafe { CFDataGetBytePtr(data.0) };
    if bytes.is_null() {
        return None;
    }
    secret_from_bytes(unsafe { slice::from_raw_parts(bytes, length as usize) })
}

pub fn delete(service: &str, account: &str) -> Result<(), String> {
    let service = string(service).ok_or_else(|| failure("delete", -1))?;
    let account = string(account).ok_or_else(|| failure("delete", -1))?;
    let key = key(service.0, account.0).ok_or_else(|| failure("delete", -1))?;
    let status = unsafe { SecItemDelete(key.0) };
    match sec_status::outcome(status) {
        Outcome::Success | Outcome::Missing => Ok(()),
        _ => Err(failure("delete", status)),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

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

//! The spell checker of Windows, the Spell Checking API of Windows 8 and later. The
//! objects of COM belong to the apartment of the thread that created them, so the thread
//! of the shared service opens that apartment and owns every checker: the callers send
//! it a word and wait for the answer, and no object of COM ever leaves that thread.

use std::ffi::c_void;
use std::ptr::null_mut;
use std::sync::OnceLock;

use crate::spelling_service::{Backend, Service, take_guesses};
use windows_sys::Win32::Globalization::SpellCheckerFactory;
use windows_sys::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
    CoUninitialize,
};
use windows_sys::core::GUID;

type HResult = i32;

const IID_SPELL_CHECKER_FACTORY: GUID = GUID::from_u128(0x8e018a9d_2415_4677_bf08_794ea61f94bb);

/// `IUnknown`, the first three slots of every interface of the API.
#[repr(C)]
struct UnknownVtbl {
    query_interface:
        unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> HResult,
    add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
}

/// `ISpellCheckerFactory`: `IUnknown`, then `get_SupportedLanguages`, `IsSupported` and
/// `CreateSpellChecker`, as `spellcheck.h` orders them.
#[repr(C)]
struct FactoryVtbl {
    query_interface:
        unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> HResult,
    add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
    get_supported_languages: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> HResult,
    is_supported: unsafe extern "system" fn(*mut c_void, *const u16, *mut i32) -> HResult,
    create_spell_checker:
        unsafe extern "system" fn(*mut c_void, *const u16, *mut *mut c_void) -> HResult,
}

/// `ISpellChecker` up to `Suggest`: `get_LanguageTag`, `Check` and `Suggest` follow
/// `IUnknown`.
#[repr(C)]
struct CheckerVtbl {
    query_interface:
        unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> HResult,
    add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
    get_language_tag: unsafe extern "system" fn(*mut c_void, *mut *mut u16) -> HResult,
    check: unsafe extern "system" fn(*mut c_void, *const u16, *mut *mut c_void) -> HResult,
    suggest: unsafe extern "system" fn(*mut c_void, *const u16, *mut *mut c_void) -> HResult,
}

/// `IEnumSpellingError`: one `Next` after `IUnknown`.
#[repr(C)]
struct EnumSpellingErrorVtbl {
    query_interface:
        unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> HResult,
    add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
    next: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> HResult,
}

/// `IEnumString`, the enumeration the suggestions come in.
#[repr(C)]
struct EnumStringVtbl {
    query_interface:
        unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> HResult,
    add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
    next: unsafe extern "system" fn(*mut c_void, u32, *mut *mut u16, *mut u32) -> HResult,
}

/// An interface of the API, released when the value goes.
struct Object {
    pointer: *mut c_void,
}

impl Object {
    fn new(pointer: *mut c_void) -> Self {
        Self { pointer }
    }
}

impl Drop for Object {
    fn drop(&mut self) {
        unsafe {
            let vtbl = vtable::<UnknownVtbl>(self.pointer);
            ((*vtbl).release)(self.pointer);
        }
    }
}

/// The apartment of COM on the thread of the checker, closed when the thread ends.
struct Apartment;

impl Apartment {
    fn start() -> Option<Self> {
        let found = unsafe { CoInitializeEx(std::ptr::null(), COINIT_MULTITHREADED as u32) };
        (found >= 0).then_some(Self)
    }
}

impl Drop for Apartment {
    fn drop(&mut self) {
        unsafe { CoUninitialize() };
    }
}

/// The checker of one language, which lives and dies on the thread of the service.
struct Checker {
    pointer: *mut c_void,
}

impl crate::spelling_service::Checker for Checker {
    fn create(language: &str) -> Option<Self> {
        let language = wide(language);
        let mut factory: *mut c_void = null_mut();
        let found = unsafe {
            CoCreateInstance(
                &SpellCheckerFactory,
                null_mut(),
                CLSCTX_INPROC_SERVER,
                &IID_SPELL_CHECKER_FACTORY,
                &mut factory,
            )
        };
        if found < 0 || factory.is_null() {
            return None;
        }
        let factory = Object::new(factory);
        let mut supported = 0i32;
        let found = unsafe {
            let vtbl = vtable::<FactoryVtbl>(factory.pointer);
            ((*vtbl).is_supported)(factory.pointer, language.as_ptr(), &mut supported)
        };
        if found < 0 || supported == 0 {
            return None;
        }
        let mut checker: *mut c_void = null_mut();
        let found = unsafe {
            let vtbl = vtable::<FactoryVtbl>(factory.pointer);
            ((*vtbl).create_spell_checker)(factory.pointer, language.as_ptr(), &mut checker)
        };
        if found < 0 || checker.is_null() {
            return None;
        }
        Some(Self { pointer: checker })
    }

    fn correct(&self, word: &str) -> bool {
        let word = wide(word);
        let mut errors: *mut c_void = null_mut();
        let found = unsafe {
            let vtbl = vtable::<CheckerVtbl>(self.pointer);
            ((*vtbl).check)(self.pointer, word.as_ptr(), &mut errors)
        };
        if found < 0 || errors.is_null() {
            return true;
        }
        let errors = Object::new(errors);
        let mut error: *mut c_void = null_mut();
        unsafe {
            let vtbl = vtable::<EnumSpellingErrorVtbl>(errors.pointer);
            ((*vtbl).next)(errors.pointer, &mut error)
        };
        // The enumeration answers one error, or none at all: a null item is a word.
        if error.is_null() {
            return true;
        }
        let _error = Object::new(error);
        false
    }

    fn suggest(&self, word: &str, limit: usize) -> Vec<String> {
        let word = wide(word);
        let mut strings: *mut c_void = null_mut();
        let found = unsafe {
            let vtbl = vtable::<CheckerVtbl>(self.pointer);
            ((*vtbl).suggest)(self.pointer, word.as_ptr(), &mut strings)
        };
        if found < 0 || strings.is_null() {
            return Vec::new();
        }
        let strings = Object::new(strings);
        take_guesses(limit, || {
            let mut text: *mut u16 = null_mut();
            let mut fetched = 0u32;
            let found = unsafe {
                let vtbl = vtable::<EnumStringVtbl>(strings.pointer);
                ((*vtbl).next)(strings.pointer, 1, &mut text, &mut fetched)
            };
            // A last item comes with `S_FALSE`, so the count decides, not the result.
            if found < 0 || fetched == 0 || text.is_null() {
                return None;
            }
            let word = unsafe { read_wide(text) };
            unsafe { CoTaskMemFree(text as *const c_void) };
            Some(word)
        })
    }
}

impl Drop for Checker {
    fn drop(&mut self) {
        unsafe {
            let vtbl = vtable::<UnknownVtbl>(self.pointer);
            ((*vtbl).release)(self.pointer);
        }
    }
}

/// The platform of this module: COM opens an apartment on the thread of the service, and
/// a request of a caller needs nothing else around it.
struct Windows;

impl Backend for Windows {
    type Opened = Apartment;
    type Checker = Checker;

    fn open() -> Option<Self::Opened> {
        Apartment::start()
    }

    fn scoped<R>(call: impl FnOnce() -> R) -> R {
        call()
    }
}

static SERVICE: OnceLock<Service> = OnceLock::new();

fn service() -> &'static Service {
    SERVICE.get_or_init(Service::start::<Windows>)
}

/// Whether the system can check `language`. The Spell Checking API of Windows answers
/// this when the checker of a language is made, and a word it cannot judge passes as
/// correct, so the source of the words stays the system on every machine.
pub fn spelling_supported(_language: &str) -> bool {
    true
}

pub fn spelling_source(language: &str) -> Option<String> {
    Some(format!("System spell checker ({language})"))
}

/// Whether the system accepts `word`, or `None` when the machine has no checker for the
/// language: the caller then takes the word as correct.
pub fn spelling_check(language: &str, word: &str) -> Option<bool> {
    service().check(language, word)
}

pub fn spelling_suggest(language: &str, word: &str, limit: usize) -> Vec<String> {
    service().suggest(language, word, limit)
}

unsafe fn vtable<T>(object: *mut c_void) -> *const T {
    unsafe { *(object as *mut *const T) }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// The text of a string of COM, up to its terminator, which the caller frees.
unsafe fn read_wide(text: *const u16) -> String {
    let mut len = 0;
    unsafe {
        while *text.add(len) != 0 {
            len += 1;
        }
        String::from_utf16_lossy(std::slice::from_raw_parts(text, len))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_checker_of_windows_answers_for_a_known_word_and_its_typo() {
        let Some(correct) = spelling_check("en-US", "calculate") else {
            return;
        };
        assert!(correct, "a dictionary of the machine knows the word");
        if let Some(correct) = spelling_check("en-US", "calcualte") {
            assert!(!correct, "the typo is not a word");
        }
        let suggestions = spelling_suggest("en-US", "calcualte", 5);
        assert!(
            suggestions.iter().all(|word| !word.is_empty()),
            "a suggestion is a word: {suggestions:?}"
        );
    }

    #[test]
    fn the_checker_answers_on_a_thread_of_a_test_too() {
        let word = "speling";
        let first = spelling_check("en-US", word);
        let second = spelling_check("en-US", "spelling");
        if let (Some(first), Some(second)) = (first, second) {
            assert!(!first, "the typo is no word");
            assert!(second, "the corrected word is a word");
        }
    }

    #[test]
    fn many_threads_share_the_one_thread_of_the_checker() {
        let callers: Vec<_> = (0..4)
            .map(|_| {
                std::thread::spawn(|| {
                    spelling_check("en-US", "calculate").unwrap_or(true)
                        && spelling_suggest("en-US", "calcualte", 5).len() <= 5
                })
            })
            .collect();
        for caller in callers {
            assert!(
                caller.join().expect("the caller of the checker ends"),
                "every thread gets its answer from the one checker"
            );
        }
    }

    #[test]
    fn the_source_names_the_system_checker() {
        assert_eq!(
            spelling_source("en-US").as_deref(),
            Some("System spell checker (en-US)")
        );
        assert!(spelling_supported("en-US"));
    }
}

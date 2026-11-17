//! The spell checker of macOS, through `NSSpellChecker` of AppKit. The class is asked
//! through the Objective-C runtime, as no other bridge to it exists in the tree. The
//! checker and every string given to it live on the one thread of the shared service,
//! and each request runs in its own autorelease pool: the guesses the system frees by
//! itself never outlive the answer.

use std::ffi::{CStr, c_char, c_void};
use std::sync::OnceLock;

use crate::spelling_service::{Backend, Service, available_tag, take_guesses};

/// An Objective-C object, which the runtime hands back as an untyped pointer.
type Id = *mut c_void;

#[link(name = "objc")]
unsafe extern "C" {
    fn objc_getClass(name: *const c_char) -> Id;
    fn sel_registerName(name: *const c_char) -> *mut c_void;
    fn objc_msgSend();
    /// The pool of the objects the runtime frees by itself, opened for one request.
    fn objc_autoreleasePoolPush() -> *mut c_void;
    fn objc_autoreleasePoolPop(pool: *mut c_void);
}

// `NSSpellChecker` is a class of AppKit, and the framework has to be loaded for
// `objc_getClass` to find it: this link is what loads it.
#[link(name = "AppKit", kind = "framework")]
unsafe extern "C" {}

/// `NSRange`, the two numbers AppKit answers a search with.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct NSRange {
    location: usize,
    length: usize,
}

/// An Objective-C object this module owns: released when it goes.
struct Object(Id);

impl Drop for Object {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { send(self.0, selector(c"release")) };
        }
    }
}

fn selector(name: &CStr) -> *mut c_void {
    unsafe { sel_registerName(name.as_ptr()) }
}

fn class(name: &CStr) -> Id {
    unsafe { objc_getClass(name.as_ptr()) }
}

/// `objc_msgSend` for a message without arguments that answers an object, as `release`
/// and `sharedSpellChecker` do.
unsafe fn send(object: Id, selector: *mut c_void) -> Id {
    let send: unsafe extern "C" fn(Id, *mut c_void) -> Id =
        unsafe { std::mem::transmute(objc_msgSend as *const ()) };
    unsafe { send(object, selector) }
}

/// `objc_msgSend` for a message with one string argument, which the caller keeps alive
/// for the call.
unsafe fn send_text(object: Id, selector: *mut c_void, text: *const c_char) -> Id {
    let send: unsafe extern "C" fn(Id, *mut c_void, *const c_char) -> Id =
        unsafe { std::mem::transmute(objc_msgSend as *const ()) };
    unsafe { send(object, selector, text) }
}

/// `objc_msgSend` for `setLanguage:`, whose `BOOL` answer is one byte.
unsafe fn send_flag(object: Id, selector: *mut c_void, argument: Id) -> i8 {
    let send: unsafe extern "C" fn(Id, *mut c_void, Id) -> i8 =
        unsafe { std::mem::transmute(objc_msgSend as *const ()) };
    unsafe { send(object, selector, argument) }
}

/// `objc_msgSend` for `count` of a collection: `NSUInteger`.
unsafe fn send_count(object: Id, selector: *mut c_void) -> usize {
    let send: unsafe extern "C" fn(Id, *mut c_void) -> usize =
        unsafe { std::mem::transmute(objc_msgSend as *const ()) };
    unsafe { send(object, selector) }
}

/// `objc_msgSend` for `objectAtIndex:`, whose index is an `NSUInteger`.
unsafe fn send_index(object: Id, selector: *mut c_void, index: usize) -> Id {
    let send: unsafe extern "C" fn(Id, *mut c_void, usize) -> Id =
        unsafe { std::mem::transmute(objc_msgSend as *const ()) };
    unsafe { send(object, selector, index) }
}

/// `objc_msgSend` for `guessesForWordRange:inString:language:inSpellDocumentWithTag:`.
unsafe fn send_guesses(
    object: Id,
    selector: *mut c_void,
    range: NSRange,
    text: Id,
    language: Id,
    tag: isize,
) -> Id {
    let send: unsafe extern "C" fn(Id, *mut c_void, NSRange, Id, Id, isize) -> Id =
        unsafe { std::mem::transmute(objc_msgSend as *const ()) };
    unsafe { send(object, selector, range, text, language, tag) }
}

/// `objc_msgSend` for
/// `checkSpellingOfString:startingAt:language:wrap:inSpellDocumentWithTag:wordCount:`,
/// which checks the whole word from its first character and asks for no count of them.
/// The range of sixteen bytes comes back in two registers on arm64, so the usual `send`
/// answers it: only a larger struct needs `objc_msgSend_stret`.
unsafe fn send_check(object: Id, selector: *mut c_void, text: Id, language: Id) -> NSRange {
    let send: unsafe extern "C" fn(
        Id,
        *mut c_void,
        Id,
        isize,
        Id,
        i8,
        isize,
        *mut isize,
    ) -> NSRange = unsafe { std::mem::transmute(objc_msgSend as *const ()) };
    let words: *mut isize = std::ptr::null_mut();
    unsafe { send(object, selector, text, 0, language, 0, 0, words) }
}

/// An `NSString` of the text, which lives as long as the value. The string is allocated
/// rather than taken from the autorelease pool, so the value owns it and releases it.
fn string(text: &str) -> Option<Object> {
    if text.as_bytes().contains(&0) {
        return None;
    }
    let mut bytes = text.as_bytes().to_vec();
    bytes.push(0);
    let class = class(c"NSString");
    if class.is_null() {
        return None;
    }
    unsafe {
        let allocated = send(class, selector(c"alloc"));
        if allocated.is_null() {
            return None;
        }
        // A string of UTF-8, which AppKit copies: the bytes live until the answer comes.
        let object = send_text(
            allocated,
            selector(c"initWithUTF8String:"),
            bytes.as_ptr().cast(),
        );
        if object.is_null() {
            // `initWithUTF8String:` has already let the allocated string go.
            return None;
        }
        Some(Object(object))
    }
}

/// The text of an `NSString`, or nothing for a null one.
fn utf8(text: Id) -> String {
    if text.is_null() {
        return String::new();
    }
    unsafe {
        let pointer = send(text, selector(c"UTF8String"));
        if pointer.is_null() {
            return String::new();
        }
        CStr::from_ptr(pointer.cast())
            .to_string_lossy()
            .into_owned()
    }
}

/// The names of the languages the system has a dictionary for.
fn available_languages(checker: Id) -> Vec<String> {
    let list = unsafe { send(checker, selector(c"availableLanguages")) };
    if list.is_null() {
        return Vec::new();
    }
    let count = unsafe { send_count(list, selector(c"count")) };
    let mut languages = Vec::new();
    for index in 0..count {
        let language = unsafe { send_index(list, selector(c"objectAtIndex:"), index) };
        let language = utf8(language);
        if !language.is_empty() {
            languages.push(language);
        }
    }
    languages
}

/// The checker of one language, which lives and dies on the thread of the service.
struct Checker {
    /// The shared checker, owned by the class rather than by this module.
    object: Id,
    /// The name of the language, as the system lists it.
    language: Object,
}

impl crate::spelling_service::Checker for Checker {
    fn create(language: &str) -> Option<Self> {
        let class = class(c"NSSpellChecker");
        if class.is_null() {
            return None;
        }
        let object = unsafe { send(class, selector(c"sharedSpellChecker")) };
        if object.is_null() {
            return None;
        }
        // The name of the dictionary the system has, out of the ones it spells its own
        // way: `en` for the `en_US` of a caller.
        let tag = available_tag(&available_languages(object), language)?;
        let tag = string(&tag)?;
        // `setLanguage:` answers whether the dictionary is there; the calls below name
        // the language themselves, so the shared checker can serve several languages.
        if unsafe { send_flag(object, selector(c"setLanguage:"), tag.0) } == 0 {
            return None;
        }
        Some(Self {
            object,
            language: tag,
        })
    }

    fn correct(&self, word: &str) -> bool {
        let Some(text) = string(word) else {
            return true;
        };
        let range = unsafe {
            send_check(
                self.object,
                selector(
                    c"checkSpellingOfString:startingAt:language:wrap:inSpellDocumentWithTag:wordCount:",
                ),
                text.0,
                self.language.0,
            )
        };
        // A misspelled word comes back as its range, and a word the system knows leaves
        // the range empty.
        range.length == 0
    }

    fn suggest(&self, word: &str, limit: usize) -> Vec<String> {
        let Some(text) = string(word) else {
            return Vec::new();
        };
        let range = NSRange {
            location: 0,
            length: word.encode_utf16().count(),
        };
        let guesses = unsafe {
            send_guesses(
                self.object,
                selector(c"guessesForWordRange:inString:language:inSpellDocumentWithTag:"),
                range,
                text.0,
                self.language.0,
                0,
            )
        };
        if guesses.is_null() {
            return Vec::new();
        }
        // The guesses come autoreleased: the value keeps them until the pool of the
        // request goes.
        let guesses = Object(unsafe { send(guesses, selector(c"retain")) });
        let count = unsafe { send_count(guesses.0, selector(c"count")) };
        let mut index = 0;
        take_guesses(limit, || {
            if index >= count {
                return None;
            }
            let guess = unsafe { send_index(guesses.0, selector(c"objectAtIndex:"), index) };
            index += 1;
            Some(utf8(guess))
        })
    }
}

/// The autorelease pool of one request: the objects the request made autoreleased go
/// when the value goes, rather than never on a thread without a pool of its own.
struct Pool(*mut c_void);

impl Pool {
    fn push() -> Self {
        Self(unsafe { objc_autoreleasePoolPush() })
    }
}

impl Drop for Pool {
    fn drop(&mut self) {
        unsafe { objc_autoreleasePoolPop(self.0) };
    }
}

/// The platform of this module: AppKit has no apartment to open, and every request runs
/// in its own pool.
struct AppKit;

impl Backend for AppKit {
    type Opened = ();
    type Checker = Checker;

    fn open() -> Option<Self::Opened> {
        Some(())
    }

    fn scoped<R>(call: impl FnOnce() -> R) -> R {
        let _pool = Pool::push();
        call()
    }
}

static SERVICE: OnceLock<Service> = OnceLock::new();

fn service() -> &'static Service {
    SERVICE.get_or_init(Service::start::<AppKit>)
}

/// Whether the machine has a dictionary for `language`: a Mac without one leaves the
/// source of words to the dictionaries of the caller, as Linux does.
pub fn spelling_supported(language: &str) -> bool {
    service().available(language)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_checker_of_the_system_knows_a_word_and_its_typo() {
        assert_eq!(
            spelling_check("en_US", "the"),
            Some(true),
            "the dictionary of the machine knows the word"
        );
        assert_eq!(
            spelling_check("en_US", "teh"),
            Some(false),
            "the typo is not a word"
        );
        let suggestions = spelling_suggest("en_US", "teh", 5);
        assert!(
            suggestions.iter().any(|word| word == "the"),
            "the correction of the typo is offered: {suggestions:?}"
        );
    }

    #[test]
    fn the_source_names_the_system_checker() {
        assert!(
            spelling_supported("en_US"),
            "a Mac has a dictionary for English"
        );
        assert_eq!(
            spelling_source("en_US").as_deref(),
            Some("System spell checker (en_US)")
        );
    }
}

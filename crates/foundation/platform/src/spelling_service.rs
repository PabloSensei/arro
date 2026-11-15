//! The one thread of the system spell checker, shared by the platforms that have one.
//! The objects of such an API belong to the thread that made them, so a single thread
//! owns every checker of every language: a caller sends it a word and waits for the
//! answer, and no object of the system ever leaves that thread. What the platform opens
//! for the thread and what it frees after a request stay in the platform's own module.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::mpsc::{Receiver, Sender, channel};

/// A checker of one language, made and asked on the thread of the service alone.
pub(crate) trait Checker: Sized {
    /// The checker of `language`, or `None` when the machine has no dictionary for it.
    fn create(language: &str) -> Option<Self>;

    /// Whether the system accepts `word`.
    fn correct(&self, word: &str) -> bool;

    /// The corrections of `word`. The service keeps no more than `limit` of them.
    fn suggest(&self, word: &str, limit: usize) -> Vec<String>;
}

/// What the thread of the service needs of its platform.
pub(crate) trait Backend {
    /// What the thread opens before the first request and closes when the queue ends:
    /// the apartment of COM of Windows.
    type Opened;
    type Checker: Checker;

    fn open() -> Option<Self::Opened>;

    /// Runs one request in what the platform frees by itself, the autorelease pool of
    /// the objects of AppKit, and answers it.
    fn scoped<R>(call: impl FnOnce() -> R) -> R;
}

/// What a caller asks the thread of the service.
pub(crate) enum Request {
    Check {
        language: String,
        word: String,
        reply: Sender<Option<bool>>,
    },
    Suggest {
        language: String,
        word: String,
        limit: usize,
        reply: Sender<Vec<String>>,
    },
    /// Whether the machine can check the language at all, which the caller asks before
    /// it chooses the source of the words. AppKit is the only system with a list of the
    /// languages it has a dictionary for.
    #[cfg(target_os = "macos")]
    Available {
        language: String,
        reply: Sender<bool>,
    },
}

/// The thread of the service and the way to reach it.
pub(crate) struct Service {
    requests: Mutex<Sender<Request>>,
}

impl Service {
    /// Starts the thread of the service. A thread that cannot be started leaves the
    /// queue closed, and every request fails at once.
    pub(crate) fn start<B: Backend>() -> Self {
        let (requests, queue) = channel();
        let spawned = std::thread::Builder::new()
            .name("spelling".to_owned())
            .spawn(move || serve::<B>(queue));
        if spawned.is_err() {
            // The caller keeps the sender and every request fails at once.
            return Self {
                requests: Mutex::new(requests),
            };
        }
        Self {
            requests: Mutex::new(requests),
        }
    }

    fn send(&self, request: Request) -> Option<()> {
        self.requests
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .send(request)
            .ok()
    }

    /// Whether the system accepts `word`, or `None` when the machine has no checker for
    /// the language: the caller then takes the word as correct.
    pub(crate) fn check(&self, language: &str, word: &str) -> Option<bool> {
        let (reply, answer) = channel();
        self.send(Request::Check {
            language: language.to_owned(),
            word: word.to_owned(),
            reply,
        })?;
        answer.recv().ok().flatten()
    }

    /// The corrections of `word`, nearest first and no more than `limit`.
    pub(crate) fn suggest(&self, language: &str, word: &str, limit: usize) -> Vec<String> {
        let (reply, answer) = channel();
        let request = Request::Suggest {
            language: language.to_owned(),
            word: word.to_owned(),
            limit,
            reply,
        };
        if self.send(request).is_none() {
            return Vec::new();
        }
        let mut suggestions = answer.recv().unwrap_or_default();
        // A checker may answer more than the caller keeps.
        suggestions.truncate(limit);
        suggestions
    }

    /// Whether the machine has a dictionary for `language` at all.
    #[cfg(target_os = "macos")]
    pub(crate) fn available(&self, language: &str) -> bool {
        let (reply, answer) = channel();
        let request = Request::Available {
            language: language.to_owned(),
            reply,
        };
        self.send(request).is_some() && answer.recv().unwrap_or(false)
    }
}

/// The loop of the thread of the service: the platform opens what the thread needs once,
/// and every language keeps its checker until the queue closes.
fn serve<B: Backend>(queue: Receiver<Request>) {
    let Some(_opened) = B::open() else {
        return;
    };
    let mut checkers: HashMap<String, Option<B::Checker>> = HashMap::new();
    while let Ok(request) = queue.recv() {
        B::scoped(|| answer::<B>(request, &mut checkers));
    }
    // The checkers go before what the thread opened closes.
    drop(checkers);
}

/// Answers one request on the thread of the service.
fn answer<B: Backend>(request: Request, checkers: &mut HashMap<String, Option<B::Checker>>) {
    match request {
        Request::Check {
            language,
            word,
            reply,
        } => {
            let correct = checker::<B>(checkers, &language).map(|it| it.correct(&word));
            reply.send(correct).ok();
        }
        Request::Suggest {
            language,
            word,
            limit,
            reply,
        } => {
            let suggestions = checker::<B>(checkers, &language)
                .map(|it| it.suggest(&word, limit))
                .unwrap_or_default();
            reply.send(suggestions).ok();
        }
        #[cfg(target_os = "macos")]
        Request::Available { language, reply } => {
            reply.send(checker::<B>(checkers, &language).is_some()).ok();
        }
    }
}

/// The checker of `language`, made on the first request for it and kept after.
fn checker<'a, B: Backend>(
    checkers: &'a mut HashMap<String, Option<B::Checker>>,
    language: &str,
) -> Option<&'a B::Checker> {
    checkers
        .entry(language.to_owned())
        .or_insert_with(|| B::Checker::create(language))
        .as_ref()
}

/// The suggestions to keep out of an answer of the system, asked one word at a time
/// until it has no more: no more than `limit`, and no empty words.
pub(crate) fn take_guesses(limit: usize, mut next: impl FnMut() -> Option<String>) -> Vec<String> {
    let mut guesses = Vec::new();
    while guesses.len() < limit {
        let Some(word) = next() else {
            break;
        };
        if !word.is_empty() {
            guesses.push(word);
        }
    }
    guesses
}

/// The name the system gives the language, out of the names it lists: the same name, or
/// a name of the same language, as the system writes `en` for the `en_US` of a caller.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn available_tag(available: &[String], language: &str) -> Option<String> {
    available
        .iter()
        .find(|tag| tag.eq_ignore_ascii_case(language))
        .or_else(|| {
            available
                .iter()
                .find(|tag| base(tag).eq_ignore_ascii_case(base(language)))
        })
        .cloned()
}

/// The language without its region: `en` of `en_US` and of `en-US`.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn base(language: &str) -> &str {
    language.split(['-', '_']).next().unwrap_or(language)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread::ThreadId;

    /// A checker of a test: it knows two words and answers more guesses than any caller
    /// keeps. It remembers the thread that made it, which every answer must stay on.
    struct TestChecker {
        thread: ThreadId,
    }

    impl Checker for TestChecker {
        fn create(language: &str) -> Option<Self> {
            (language == "en").then(|| Self {
                thread: std::thread::current().id(),
            })
        }

        fn correct(&self, word: &str) -> bool {
            assert_eq!(
                self.thread,
                std::thread::current().id(),
                "the checker answers on the thread that made it"
            );
            matches!(word, "the" | "spell")
        }

        fn suggest(&self, _word: &str, limit: usize) -> Vec<String> {
            (0..limit + 2)
                .map(|index| format!("guess {index}"))
                .collect()
        }
    }

    /// The backend of the tests: the service without a system under it.
    struct TestBackend;

    impl Backend for TestBackend {
        type Opened = ();
        type Checker = TestChecker;

        fn open() -> Option<Self::Opened> {
            Some(())
        }

        fn scoped<R>(call: impl FnOnce() -> R) -> R {
            call()
        }
    }

    fn service() -> Service {
        Service::start::<TestBackend>()
    }

    #[test]
    fn the_thread_of_the_service_answers_a_word_and_its_typo() {
        let service = service();
        assert_eq!(service.check("en", "spell"), Some(true));
        assert_eq!(service.check("en", "teh"), Some(false));
    }

    #[test]
    fn a_language_without_a_checker_answers_nothing() {
        let service = service();
        assert_eq!(service.check("de", "spell"), None);
        assert!(service.suggest("de", "teh", 5).is_empty());
        assert_eq!(
            service.check("en", "spell"),
            Some(true),
            "the queue keeps working after a language without a checker"
        );
    }

    #[test]
    fn the_suggestions_stop_at_the_limit() {
        let service = service();
        let suggestions = service.suggest("en", "teh", 2);
        assert_eq!(suggestions, ["guess 0", "guess 1"]);
    }

    #[test]
    fn many_threads_share_the_one_thread_of_the_service() {
        let service = service();
        std::thread::scope(|scope| {
            for _ in 0..4 {
                scope.spawn(|| {
                    assert_eq!(service.check("en", "spell"), Some(true));
                    assert_eq!(service.suggest("en", "teh", 2).len(), 2);
                });
            }
        });
    }

    #[test]
    fn the_guesses_keep_the_limit_and_drop_the_empty_ones() {
        let mut words = ["the", "", "then", "there"].into_iter().map(str::to_owned);
        assert_eq!(take_guesses(2, || words.next()), ["the", "then"]);
        assert!(take_guesses(5, || None).is_empty());
    }

    #[test]
    fn the_language_is_found_by_its_name_or_by_its_language() {
        let available = ["en".to_owned(), "de_DE".to_owned()];
        assert_eq!(available_tag(&available, "en_US").as_deref(), Some("en"));
        assert_eq!(available_tag(&available, "de-AT").as_deref(), Some("de_DE"));
        assert_eq!(available_tag(&available, "fr"), None);
        assert_eq!(available_tag(&[], "en"), None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_language_without_a_checker_is_not_available() {
        let service = service();
        assert!(service.available("en"));
        assert!(!service.available("fr"));
    }
}

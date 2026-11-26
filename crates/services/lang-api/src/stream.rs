//! The contract of every provider: a query returns at once and answers arrive as a stream.
//!
//! One request may answer twice, a quick native answer first and a refinement later, so
//! `NativeThenLsp` needs nothing beyond this. The caller never waits on a provider: it
//! polls [`Replies`] or awaits it on an executor. Dropping `Replies` cancels the request.

use std::collections::VecDeque;
use std::fmt;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::task::{Context, Poll, Waker};

use base::Cancel;
use futures_core::Stream;

use crate::Answer;

/// Answers one request may hold before the caller reads them. A later answer replaces
/// the oldest one, so a caller that fell behind still sees the newest state.
const CAPACITY: usize = 4;

/// One capability of a language. `start` must not block: work goes to the provider's own
/// threads, which send into `reply` and drop it when done.
pub trait Provider<Q, T>: Send + Sync {
    fn start(&self, query: Q, cancel: Cancel, reply: Reply<T>);
}

/// Runs a query and hands back the stream of its answers.
pub fn ask<Q, T>(provider: &dyn Provider<Q, T>, query: Q) -> Replies<T> {
    let (reply, replies) = channel();
    provider.start(query, replies.cancel.clone(), reply);
    replies
}

pub fn channel<T>() -> (Reply<T>, Replies<T>) {
    let cancel = Cancel::new();
    let shared = Arc::new(Shared {
        answers: Mutex::new(Answers {
            queue: VecDeque::new(),
            waker: None,
            receiver_gone: false,
            sender_gone: false,
        }),
    });
    let reply = Reply {
        shared: shared.clone(),
        cancel: cancel.clone(),
    };
    (reply, Replies { shared, cancel })
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

struct Shared<T> {
    answers: Mutex<Answers<T>>,
}

struct Answers<T> {
    queue: VecDeque<Answer<T>>,
    /// The caller that waits for the next answer.
    waker: Option<Waker>,
    receiver_gone: bool,
    sender_gone: bool,
}

/// The provider's end. The stream ends when this is dropped.
pub struct Reply<T> {
    shared: Arc<Shared<T>>,
    cancel: Cancel,
}

#[derive(Debug, PartialEq, Eq)]
pub struct ReplyClosed;

impl fmt::Display for ReplyClosed {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("the request is gone or its reply queue is full")
    }
}

impl std::error::Error for ReplyClosed {}

impl<T> Reply<T> {
    /// Never waits. A provider stops working on `ReplyClosed`. The answer of a full
    /// queue still replaces the oldest one before the failure is reported, so the
    /// caller sees the latest state even when it reads only after the provider stopped.
    pub fn send(&mut self, answer: Answer<T>) -> Result<(), ReplyClosed> {
        let (full, waker) = {
            let mut answers = lock(&self.shared.answers);
            if answers.receiver_gone {
                return Err(ReplyClosed);
            }
            let full = answers.queue.len() >= CAPACITY;
            if full {
                answers.queue.pop_front();
            }
            answers.queue.push_back(answer);
            (full, answers.waker.take())
        };
        if let Some(waker) = waker {
            waker.wake();
        }
        if full { Err(ReplyClosed) } else { Ok(()) }
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled() || lock(&self.shared.answers).receiver_gone
    }
}

impl<T> Drop for Reply<T> {
    fn drop(&mut self) {
        let waker = {
            let mut answers = lock(&self.shared.answers);
            answers.sender_gone = true;
            answers.waker.take()
        };
        if let Some(waker) = waker {
            waker.wake();
        }
    }
}

/// The caller's end, a stream of answers ending when the provider is done.
pub struct Replies<T> {
    shared: Arc<Shared<T>>,
    cancel: Cancel,
}

impl<T> Replies<T> {
    /// The next answer if one is ready, for a caller that may not wait.
    pub fn poll_ready(&mut self) -> Option<Answer<T>> {
        lock(&self.shared.answers).queue.pop_front()
    }
}

impl<T> Stream for Replies<T> {
    type Item = Answer<T>;

    fn poll_next(self: Pin<&mut Self>, context: &mut Context) -> Poll<Option<Self::Item>> {
        let mut answers = lock(&self.shared.answers);
        if let Some(answer) = answers.queue.pop_front() {
            return Poll::Ready(Some(answer));
        }
        if answers.sender_gone {
            return Poll::Ready(None);
        }
        answers.waker = Some(context.waker().clone());
        Poll::Pending
    }
}

impl<T> Drop for Replies<T> {
    fn drop(&mut self) {
        self.cancel.cancel();
        let waker = {
            let mut answers = lock(&self.shared.answers);
            answers.receiver_gone = true;
            answers.queue.clear();
            answers.waker.take()
        };
        if let Some(waker) = waker {
            waker.wake();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc as std_mpsc;
    use std::thread;

    use futures_executor::block_on;
    use futures_util::StreamExt;

    use super::*;
    use crate::{Confidence, SourceId};

    const NATIVE: SourceId = SourceId::new("native");
    const SERVER: SourceId = SourceId::new("server");

    /// Answers twice from its own thread: at once, then with the refinement.
    struct TwoPhases;

    impl Provider<u32, u32> for TwoPhases {
        fn start(&self, query: u32, _cancel: Cancel, mut reply: Reply<u32>) {
            thread::spawn(move || {
                reply
                    .send(Answer::new(query, Confidence::Partial, NATIVE))
                    .unwrap();
                reply
                    .send(Answer::new(query + 1, Confidence::Certain, SERVER))
                    .unwrap();
            });
        }
    }

    #[test]
    fn a_request_gets_a_quick_answer_and_then_the_refinement() {
        let answers: Vec<_> = block_on(ask(&TwoPhases, 1).collect());

        assert_eq!(
            answers,
            [
                Answer::new(1, Confidence::Partial, NATIVE),
                Answer::new(2, Confidence::Certain, SERVER),
            ]
        );
    }

    #[test]
    fn a_caller_that_may_not_wait_sees_no_answer_yet() {
        let (mut reply, mut replies) = channel::<u32>();
        assert_eq!(replies.poll_ready(), None);

        reply
            .send(Answer::new(1, Confidence::Certain, NATIVE))
            .unwrap();

        assert_eq!(
            replies.poll_ready(),
            Some(Answer::new(1, Confidence::Certain, NATIVE))
        );
    }

    /// Reports to the test once it sees the request cancelled.
    struct Watches(std_mpsc::Sender<()>);

    impl Provider<(), ()> for Watches {
        fn start(&self, _query: (), cancel: Cancel, reply: Reply<()>) {
            let done = self.0.clone();
            thread::spawn(move || {
                while !cancel.is_cancelled() {
                    thread::yield_now();
                }
                drop(reply);
                done.send(()).unwrap();
            });
        }
    }

    #[test]
    fn dropping_the_stream_cancels_the_request() {
        let (done, cancelled) = std_mpsc::channel();

        drop(ask(&Watches(done), ()));

        cancelled.recv().unwrap();
    }

    #[test]
    fn a_provider_learns_that_nobody_listens() {
        let (mut reply, replies) = channel::<u32>();
        drop(replies);

        assert!(reply.is_cancelled());
        assert_eq!(
            reply.send(Answer::new(1, Confidence::Certain, NATIVE)),
            Err(ReplyClosed)
        );
    }

    #[test]
    fn a_full_queue_refuses_instead_of_waiting() {
        let (mut reply, _replies) = channel::<usize>();

        let sent = (0..100)
            .map(|index| reply.send(Answer::new(index, Confidence::Partial, NATIVE)))
            .take_while(Result::is_ok)
            .count();

        assert!(sent < 100);
    }
}

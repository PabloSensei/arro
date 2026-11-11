//! The contract of every provider: a query returns at once and answers arrive as a stream.
//!
//! One request may answer twice, a quick native answer first and a refinement later, so
//! `NativeThenLsp` needs nothing beyond this. The caller never waits on a provider: it
//! polls [`Replies`] or awaits it on an executor. Dropping `Replies` cancels the request.

use std::fmt;
use std::pin::Pin;
use std::task::{Context, Poll};

use base::Cancel;
use futures_channel::mpsc;
use futures_core::Stream;

use crate::Answer;

/// Answers one request may send before the caller reads them. A later answer replaces an
/// earlier one, so a request has no use for a long queue.
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
    let (sender, receiver) = mpsc::channel(CAPACITY);
    let reply = Reply {
        sender,
        cancel: cancel.clone(),
    };
    (reply, Replies { receiver, cancel })
}

/// The provider's end. The stream ends when this is dropped.
pub struct Reply<T> {
    sender: mpsc::Sender<Answer<T>>,
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
    /// Never waits. A provider stops working on `ReplyClosed`.
    pub fn send(&mut self, answer: Answer<T>) -> Result<(), ReplyClosed> {
        self.sender.try_send(answer).map_err(|_| ReplyClosed)
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled() || self.sender.is_closed()
    }
}

/// The caller's end, a stream of answers ending when the provider is done.
pub struct Replies<T> {
    receiver: mpsc::Receiver<Answer<T>>,
    cancel: Cancel,
}

impl<T> Replies<T> {
    /// The next answer if one is ready, for a caller that may not wait.
    pub fn poll_ready(&mut self) -> Option<Answer<T>> {
        self.receiver.try_recv().ok()
    }
}

impl<T> Stream for Replies<T> {
    type Item = Answer<T>;

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.receiver).poll_next(context)
    }
}

impl<T> Drop for Replies<T> {
    fn drop(&mut self) {
        self.cancel.cancel();
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

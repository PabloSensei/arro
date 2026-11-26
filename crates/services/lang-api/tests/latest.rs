//! A provider that answers faster than the caller reads: the newest answer wins.

use futures_executor::block_on;
use futures_util::StreamExt;
use lang_api::{Answer, Confidence, ReplyClosed, SourceId, channel};

const SOURCE: SourceId = SourceId::new("test");

#[test]
fn ten_answers_with_a_busy_receiver_leave_the_last_one() {
    let (mut reply, replies) = channel::<usize>();

    for value in 0..10 {
        // A full queue reports the drop of an older answer, and the newest one still
        // replaces it: a provider that keeps sending is not the only way to see it.
        let _: Result<(), ReplyClosed> =
            reply.send(Answer::new(value, Confidence::Partial, SOURCE));
    }
    drop(reply);

    let answers: Vec<_> = block_on(replies.collect());
    assert_eq!(
        answers.last().map(|answer| answer.value),
        Some(9),
        "the caller sees the answer of the latest state"
    );
}

#[test]
fn the_newest_answer_replaces_the_oldest_when_the_queue_is_full() {
    let (mut reply, replies) = channel::<usize>();
    for value in 0..6 {
        let _ = reply.send(Answer::new(value, Confidence::Partial, SOURCE));
    }
    drop(reply);

    let answers: Vec<_> = block_on(replies.collect());
    assert_eq!(
        answers
            .iter()
            .map(|answer| answer.value)
            .collect::<Vec<_>>(),
        [2, 3, 4, 5],
        "the oldest answers are the ones a late caller cannot use"
    );
}

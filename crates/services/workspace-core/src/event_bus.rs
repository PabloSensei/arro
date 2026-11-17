use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll};

use futures_channel::mpsc;
use futures_core::Stream;

/// Delivers every event to every subscriber. Publishing never waits: the thread of the UI
/// publishes, and a stuck subscriber must not stop it. A subscriber that falls behind
/// loses events and learns about it, see [`Delivery::Lost`].
pub struct EventBus<E> {
    subscribers: Arc<Mutex<Vec<Subscriber<E>>>>,
}

struct Subscriber<E> {
    sender: mpsc::Sender<E>,
    lost: Arc<AtomicU64>,
}

impl<E> Clone for EventBus<E> {
    fn clone(&self) -> Self {
        Self {
            subscribers: self.subscribers.clone(),
        }
    }
}

impl<E> Default for EventBus<E> {
    fn default() -> Self {
        Self {
            subscribers: Arc::default(),
        }
    }
}

impl<E: Clone> EventBus<E> {
    pub fn new() -> Self {
        Self::default()
    }

    /// `capacity` is how far this subscriber may fall behind before it loses events.
    pub fn subscribe(&self, capacity: usize) -> Subscription<E> {
        let (sender, receiver) = mpsc::channel(capacity);
        let lost = Arc::new(AtomicU64::new(0));
        let subscriber = Subscriber {
            sender,
            lost: lost.clone(),
        };
        self.lock().push(subscriber);
        Subscription { receiver, lost }
    }

    pub fn publish(&self, event: E) {
        self.lock().retain_mut(
            |subscriber| match subscriber.sender.try_send(event.clone()) {
                Ok(()) => true,
                Err(error) if error.is_full() => {
                    subscriber.lost.fetch_add(1, Ordering::Relaxed);
                    true
                }
                Err(_) => false,
            },
        );
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<Subscriber<E>>> {
        // Sending cannot leave the list half changed, so a panic elsewhere changes nothing.
        self.subscribers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Delivery<E> {
    Event(E),
    /// This many events did not fit. Read the current state again instead of replaying.
    Lost(u64),
}

pub struct Subscription<E> {
    receiver: mpsc::Receiver<E>,
    lost: Arc<AtomicU64>,
}

impl<E> Subscription<E> {
    /// What is ready now, for a caller that may not wait.
    pub fn try_next(&mut self) -> Option<Delivery<E>> {
        match self.receiver.try_recv() {
            Ok(event) => Some(Delivery::Event(event)),
            Err(_) => self.take_lost(),
        }
    }

    fn take_lost(&self) -> Option<Delivery<E>> {
        let lost = self.lost.swap(0, Ordering::Relaxed);
        (lost > 0).then_some(Delivery::Lost(lost))
    }
}

impl<E> Stream for Subscription<E> {
    type Item = Delivery<E>;

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context) -> Poll<Option<Self::Item>> {
        match Pin::new(&mut self.receiver).poll_next(context) {
            Poll::Ready(Some(event)) => Poll::Ready(Some(Delivery::Event(event))),
            Poll::Ready(None) => Poll::Ready(self.take_lost()),
            Poll::Pending => self
                .take_lost()
                .map_or(Poll::Pending, |lost| Poll::Ready(Some(lost))),
        }
    }
}

#[cfg(test)]
mod tests {
    use futures_executor::block_on;
    use futures_util::StreamExt;

    use super::*;

    #[test]
    fn every_subscriber_gets_every_event() {
        let bus = EventBus::new();
        let mut first = bus.subscribe(4);
        let mut second = bus.subscribe(4);

        bus.publish(1);

        assert_eq!(first.try_next(), Some(Delivery::Event(1)));
        assert_eq!(second.try_next(), Some(Delivery::Event(1)));
        assert_eq!(first.try_next(), None);
    }

    #[test]
    fn a_slow_subscriber_loses_events_and_is_told_so() {
        let bus = EventBus::new();
        let mut slow = bus.subscribe(1);
        let mut quick = bus.subscribe(100);

        for event in 0..10 {
            bus.publish(event);
        }

        let mut seen = Vec::new();
        while let Some(delivery) = slow.try_next() {
            seen.push(delivery);
        }
        let Some(Delivery::Lost(lost)) = seen.pop() else {
            panic!("no report of lost events in {seen:?}");
        };
        assert_eq!(seen.len() as u64 + lost, 10);
        let mut delivered = 0;
        while quick.try_next().is_some() {
            delivered += 1;
        }
        assert_eq!(delivered, 10);
    }

    #[test]
    fn a_dropped_subscription_leaves_the_bus() {
        let bus = EventBus::new();
        drop(bus.subscribe(1));

        bus.publish(1);

        assert!(bus.lock().is_empty());
    }

    #[test]
    fn a_subscription_is_a_stream() {
        let bus = EventBus::new();
        let mut subscription = bus.subscribe(4);

        bus.publish(1);

        assert_eq!(block_on(subscription.next()), Some(Delivery::Event(1)));
    }
}

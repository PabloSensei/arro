use std::sync::Arc;

use arc_swap::ArcSwap;

/// The latest finished state of something built in the background. The UI thread reads it
/// without waiting for a lock: a writer replaces the whole value by swapping a pointer.
pub struct Snapshot<T> {
    current: ArcSwap<T>,
}

impl<T> Snapshot<T> {
    pub fn new(value: T) -> Self {
        Self {
            current: ArcSwap::from_pointee(value),
        }
    }

    pub fn get(&self) -> Arc<T> {
        self.current.load_full()
    }

    pub fn set(&self, value: T) {
        self.current.store(Arc::new(value));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reader_keeps_the_value_it_took() {
        let snapshot = Snapshot::new(1);
        let taken = snapshot.get();

        snapshot.set(2);

        assert_eq!(*taken, 1);
        assert_eq!(*snapshot.get(), 2);
    }
}

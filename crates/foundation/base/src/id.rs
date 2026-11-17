use std::cmp::Ordering;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::marker::PhantomData;
use std::sync::atomic::{self, AtomicU64};

/// An id that does not mix with ids of other things: `Id<Document>` is not `Id<Session>`.
/// The traits are written by hand because derives would demand them of `T`.
pub struct Id<T> {
    raw: u64,
    kind: PhantomData<fn() -> T>,
}

impl<T> Id<T> {
    pub const fn from_raw(raw: u64) -> Self {
        Self {
            raw,
            kind: PhantomData,
        }
    }

    pub const fn raw(self) -> u64 {
        self.raw
    }
}

impl<T> Clone for Id<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for Id<T> {}

impl<T> PartialEq for Id<T> {
    fn eq(&self, other: &Self) -> bool {
        self.raw == other.raw
    }
}

impl<T> Eq for Id<T> {}

impl<T> PartialOrd for Id<T> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl<T> Ord for Id<T> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.raw.cmp(&other.raw)
    }
}

impl<T> Hash for Id<T> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.raw.hash(state);
    }
}

impl<T> fmt::Debug for Id<T> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "Id({})", self.raw)
    }
}

/// Hands out ids unique within this source, from any thread.
pub struct IdSource<T> {
    next: AtomicU64,
    kind: PhantomData<fn() -> T>,
}

impl<T> IdSource<T> {
    pub const fn new() -> Self {
        Self {
            next: AtomicU64::new(1),
            kind: PhantomData,
        }
    }

    pub fn next(&self) -> Id<T> {
        Id::from_raw(self.next.fetch_add(1, atomic::Ordering::Relaxed))
    }
}

impl<T> Default for IdSource<T> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Document;

    #[test]
    fn a_source_never_repeats_an_id() {
        let source = IdSource::<Document>::new();
        let first = source.next();
        let second = source.next();

        assert_ne!(first, second);
        assert!(first < second);
    }

    #[test]
    fn an_id_needs_nothing_from_its_kind() {
        let id = Id::<Document>::from_raw(7);
        let copy = id;

        assert_eq!(id, copy);
        assert_eq!(format!("{id:?}"), "Id(7)");
    }
}

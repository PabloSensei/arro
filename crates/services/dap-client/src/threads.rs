//! The thread list the window draws. A debuggee may run thousands of virtual threads,
//! so the list is one row for each platform thread and one collapsed group of pages for
//! the virtual ones.
//!
//! The threads response of a debug adapter carries no stack frames: asking every thread
//! for a stack would be the flood the grouping exists to prevent. Adapters that add a
//! top frame to a thread could group by it; this list pages instead.

use crate::model::Thread;

/// How many virtual threads one page shows.
pub const VIRTUAL_PAGE: usize = 50;

/// The platform threads and the pages of the virtual threads, with the stopped thread
/// kept apart on top.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ThreadList {
    /// The thread the session stopped on, always drawn first and selected.
    pub stopped: Option<Thread>,
    /// Platform threads in the order the adapter sent them.
    pub platform: Vec<Thread>,
    /// The virtual threads of the current page.
    pub virtual_threads: VirtualThreads,
}

/// One page of the virtual threads and how many there are in all.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VirtualThreads {
    /// Every virtual thread the adapter reported, including the stopped one when it is
    /// virtual and stays on top of the list.
    pub total: usize,
    /// The shown page, from zero.
    pub page: usize,
    /// The number of pages, zero when there is no virtual thread.
    pub pages: usize,
    /// The threads of the shown page.
    pub threads: Vec<Thread>,
}

impl VirtualThreads {
    /// The label of the collapsed row.
    pub fn label(&self) -> String {
        format!("Virtual threads ({})", self.total)
    }

    /// Whether the group has more than one page.
    pub fn has_pages(&self) -> bool {
        self.pages > 1
    }
}

/// The default thread names of the JDK contain this marker. The debug adapter is free
/// to send another name, and a named virtual thread cannot be told from a platform one
/// by name alone.
pub fn is_virtual_thread(name: &str) -> bool {
    let name = name.trim();
    let name = name
        .strip_prefix("Thread [")
        .and_then(|rest| rest.strip_suffix(']'))
        .unwrap_or(name);
    name.starts_with("VirtualThread[#")
}

/// Splits the threads into the stopped one, the platform ones and one page of the
/// virtual ones. `page` is clamped to the last page.
pub fn thread_list(threads: &[Thread], current: Option<i64>, page: usize) -> ThreadList {
    let stopped = current.and_then(|id| {
        threads
            .iter()
            .find(|thread| thread.id == id)
            .cloned()
            .or_else(|| {
                // The adapter may report a stop before its thread list knows the thread.
                Some(Thread {
                    id,
                    name: format!("thread #{id}"),
                })
            })
    });
    let mut platform = Vec::new();
    let mut virtuals = Vec::new();
    for thread in threads {
        if Some(thread.id) == current {
            continue;
        }
        if is_virtual_thread(&thread.name) {
            virtuals.push(thread.clone());
        } else {
            platform.push(thread.clone());
        }
    }
    let pages = virtuals.len().div_ceil(VIRTUAL_PAGE);
    let page = page.min(pages.saturating_sub(1));
    let start = page * VIRTUAL_PAGE;
    let shown = virtuals
        .iter()
        .skip(start)
        .take(VIRTUAL_PAGE)
        .cloned()
        .collect();
    ThreadList {
        stopped,
        platform,
        virtual_threads: VirtualThreads {
            total: virtuals.len(),
            page,
            pages,
            threads: shown,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn thread(id: i64, name: &str) -> Thread {
        Thread {
            id,
            name: name.to_owned(),
        }
    }

    #[test]
    fn a_virtual_thread_is_recognised_by_its_default_name() {
        assert!(is_virtual_thread(
            "VirtualThread[#21]/runnable@ForkJoinPool-1-worker-3"
        ));
        assert!(is_virtual_thread(
            "Thread [VirtualThread[#21]/runnable@ForkJoinPool-1-worker-3]"
        ));
        assert!(!is_virtual_thread("Thread [main]"));
        assert!(!is_virtual_thread("worker"));
        assert!(!is_virtual_thread("virtual-thread-1"));
    }

    #[test]
    fn ten_thousand_virtual_threads_are_pages_with_the_stopped_thread_on_top() {
        let mut threads = Vec::with_capacity(10_020);
        threads.push(thread(1, "main"));
        for id in 2..=20 {
            threads.push(thread(id, "worker"));
        }
        for id in 21..=10_020 {
            threads.push(thread(
                id,
                &format!("VirtualThread[#{id}]/runnable@ForkJoinPool-1-worker-1"),
            ));
        }

        let list = thread_list(&threads, Some(21), 0);

        assert_eq!(list.stopped.as_ref().map(|thread| thread.id), Some(21));
        assert_eq!(list.platform.len(), 20);
        assert_eq!(list.virtual_threads.total, 9_999);
        assert_eq!(list.virtual_threads.page, 0);
        assert_eq!(list.virtual_threads.pages, 200);
        assert_eq!(list.virtual_threads.threads.len(), VIRTUAL_PAGE);
        assert!(
            list.virtual_threads
                .threads
                .iter()
                .all(|thread| thread.id != 21)
        );

        let last = thread_list(&threads, Some(21), 999);
        assert_eq!(last.virtual_threads.page, 199);
        assert_eq!(last.virtual_threads.threads.len(), 49);
        assert_eq!(
            last.virtual_threads.threads.last().map(|t| t.id),
            Some(10_020)
        );
    }

    #[test]
    fn a_stop_before_the_thread_list_does_not_lose_the_thread() {
        let list = thread_list(&[], Some(7), 0);

        assert_eq!(list.stopped, Some(thread(7, "thread #7")));
        assert!(list.platform.is_empty());
        assert_eq!(list.virtual_threads.total, 0);
    }
}

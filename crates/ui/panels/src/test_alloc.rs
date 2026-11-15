//! The trap that names a test which asks for a huge allocation. A full parallel run of
//! the window tests once died with `memory allocation of 21474836480 bytes failed` and no
//! test name: the process goes away before libtest prints which test was running. Every
//! request above `LIMIT` prints its size, the thread name (for libtest it is the test) and
//! a backtrace, then aborts, so the next such case points at its test and its call stack.
//!
//! The whole window suite stays far below the limit: the biggest single request of any
//! test is about 10 MiB (a measurement of the debug test build, T-0225). A request above
//! a gigabyte is a bug, not a need.

use std::alloc::{GlobalAlloc, Layout, System};
use std::backtrace::Backtrace;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};

/// A request above this asks for far more than any window test needs.
const LIMIT: usize = 1024 * 1024 * 1024;

/// Set while the report itself allocates, so its own memory is not reported. It is a
/// process-wide flag and not a thread-local one: a thread-local cannot be read while the
/// thread runs its destructors, and the report allocates on the way out.
static REPORTING: AtomicBool = AtomicBool::new(false);

/// The system allocator with the trap in front of it, for the test binary only.
struct Trap;

#[global_allocator]
static ALLOCATOR: Trap = Trap;

unsafe impl GlobalAlloc for Trap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        report(&layout);
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        report(&layout);
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // A growth above `isize::MAX` is refused by `Vec` itself before it comes here.
        if let Ok(grown) = Layout::from_size_align(new_size, layout.align()) {
            report(&grown);
        }
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

/// Prints the request and the test that made it, then aborts: unwinding out of an
/// allocator is not a thing to do.
fn report(layout: &Layout) {
    if layout.size() < LIMIT || REPORTING.swap(true, Ordering::Relaxed) {
        return;
    }
    let thread = std::thread::current();
    let mut stderr = std::io::stderr().lock();
    let _ = writeln!(
        stderr,
        "huge allocation of {} bytes in test {}",
        layout.size(),
        thread.name().unwrap_or("<unnamed>")
    );
    let _ = writeln!(stderr, "{}", Backtrace::force_capture());
    let _ = stderr.flush();
    drop(stderr);
    std::process::abort();
}

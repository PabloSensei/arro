//! A minidump for crashes that never reach the panic hook: an access violation, a stack
//! overflow, or an abort. The dump goes to the same log folder as the panic reports, and
//! the crash then continues to Windows as before.
//!
//! A stack overflow leaves almost no stack for the filter, so the dump is written by a
//! thread that `install` starts and parks on an event. The filter only hands over the
//! exception and waits for the writer.

use std::ffi::c_int;
use std::io;
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use windows_sys::Win32::Foundation::{
    CloseHandle, EXCEPTION_STACK_OVERFLOW, HANDLE, HMODULE, WAIT_OBJECT_0,
};
use windows_sys::Win32::System::Diagnostics::Debug::{
    EXCEPTION_CONTINUE_SEARCH, EXCEPTION_POINTERS, MINIDUMP_EXCEPTION_INFORMATION, MINIDUMP_TYPE,
    MiniDumpNormal, MiniDumpWithDataSegs, MiniDumpWithThreadInfo, MiniDumpWriteDump,
    SetUnhandledExceptionFilter,
};
use windows_sys::Win32::System::LibraryLoader::{
    GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
    GetModuleFileNameW, GetModuleHandleExW,
};
use windows_sys::Win32::System::Threading::{
    CreateEventW, GetCurrentProcess, GetCurrentProcessId, GetCurrentThreadId, INFINITE, SetEvent,
    WaitForSingleObject,
};

/// How many crashes stay on disk; a crash owns one dump and one text report.
const KEEP_CRASHES: usize = 10;

/// How long the crashing thread waits for the writer before it lets Windows continue.
const WRITER_TIMEOUT_MS: u32 = 30_000;

/// The value of `SIGABRT` in the Microsoft C runtime.
const SIGABRT: c_int = 22;
/// `SIG_DFL` and `SIG_IGN` from the Microsoft C runtime.
const SIG_DFL: usize = 0;
const SIG_IGN: usize = 1;

/// The folder is installed once; the handlers are plain functions without context.
static CRASH_DIR: OnceLock<PathBuf> = OnceLock::new();
static CRASH_WRITER: OnceLock<Writer> = OnceLock::new();
/// The program that reports a crash; `install` receives it from the application.
static IDENTITY: OnceLock<Identity> = OnceLock::new();
static PREVIOUS_ABORT: AtomicUsize = AtomicUsize::new(SIG_DFL);
static PREVIOUS_FILTER: AtomicUsize = AtomicUsize::new(0);
static CRASH_WRITING: WriteGate = WriteGate::new();

/// `MiniDumpWriteDump` is single threaded: its documentation says that a caller must
/// synchronize the calls of the process. Two dumps at once, as a test of this module and
/// the writer thread of a crash can take, fail with `ERROR_BAD_LENGTH` or hang in DbgHelp.
static DUMP_LOCK: Mutex<()> = Mutex::new(());

unsafe extern "C" {
    /// The CRT handler slot for `SIGABRT`. `abort` raises the signal, so this catches an
    /// abort that would otherwise only leave an exit code.
    fn signal(signal: c_int, handler: usize) -> usize;
}

/// The version and commit of the binary, which the report names before the exception.
struct Identity {
    version: String,
    commit: String,
}

/// Writes a minidump and a short report on an unhandled exception or an abort.
pub fn install(dir: PathBuf, version: String, commit: String) -> io::Result<()> {
    let _ = IDENTITY.set(Identity { version, commit });
    let writer_dir = dir.clone();
    let _ = CRASH_DIR.set(dir);
    if let Some(dir) = CRASH_DIR.get() {
        // Old dumps from before this run must not stay forever.
        let _ = prune(dir, KEEP_CRASHES);
    }
    if CRASH_WRITER.get().is_none() {
        let _ = CRASH_WRITER.set(Writer::spawn(writer_dir)?);
    }
    // SAFETY: the filter is a function that lives for the whole process, and passing it
    // replaces the default WER prompt for this process only. The returned filter, if any,
    // is called after our write so that Windows still handles the crash as before.
    let previous = unsafe { SetUnhandledExceptionFilter(Some(handle_unhandled_exception)) };
    PREVIOUS_FILTER.store(
        previous.map_or(0, |filter| filter as usize),
        Ordering::Relaxed,
    );
    install_abort_handler()
}

fn install_abort_handler() -> io::Result<()> {
    // SAFETY: the handler is a function that lives for the whole process; a signal handler
    // runs on the thread that raises the signal.
    let previous = unsafe { signal(SIGABRT, handle_abort as *const () as usize) };
    if previous == usize::MAX {
        return Err(io::Error::last_os_error());
    }
    PREVIOUS_ABORT.store(previous, Ordering::Relaxed);
    Ok(())
}

unsafe extern "system" fn handle_unhandled_exception(pointers: *const EXCEPTION_POINTERS) -> i32 {
    // SAFETY: the filter runs on the crashed thread, so this is the thread to record.
    let thread_id = unsafe { GetCurrentThreadId() };
    write_from_handler(pointers, thread_id);
    let previous = PREVIOUS_FILTER.load(Ordering::Relaxed);
    if previous != 0 {
        // SAFETY: the value came back from `SetUnhandledExceptionFilter`, so it is a filter
        // of the same type, and calling it after our write preserves its answer.
        let previous: unsafe extern "system" fn(*const EXCEPTION_POINTERS) -> i32 =
            unsafe { std::mem::transmute(previous) };
        return unsafe { previous(pointers) };
    }
    // Let Windows handle the crash as usual now that the evidence is on disk.
    EXCEPTION_CONTINUE_SEARCH
}

unsafe extern "C" fn handle_abort(number: c_int) {
    // SAFETY: the CRT handler runs on the thread that raised the signal.
    let thread_id = unsafe { GetCurrentThreadId() };
    write_from_handler(std::ptr::null(), thread_id);
    let previous = PREVIOUS_ABORT.load(Ordering::Relaxed);
    if previous != SIG_DFL && previous != SIG_IGN {
        // SAFETY: the value came back from `signal`, so it is a handler of the same type.
        let previous: unsafe extern "C" fn(c_int) = unsafe { std::mem::transmute(previous) };
        // SAFETY: the CRT handler is running, and chaining is what a second installer expects.
        unsafe { previous(number) };
    }
}

/// Hands one crash to the writer thread. A second crash while the first write is in
/// progress does not write again.
fn write_from_handler(pointers: *const EXCEPTION_POINTERS, thread_id: u32) {
    if !CRASH_WRITING.begin() {
        return;
    }
    if let Some(writer) = CRASH_WRITER.get() {
        writer.request(pointers, thread_id);
        let _ = writer.wait();
        return;
    }
    if let Some(dir) = CRASH_DIR.get() {
        let _ = write_crash(dir, pointers, thread_id);
    }
}

/// A pre-created thread that owns the file work, so a stack overflow on the crashed
/// thread does not prevent the dump. It serves one crash and then exits.
#[derive(Clone)]
struct Writer {
    inner: Arc<WriterInner>,
}

struct WriterInner {
    dir: PathBuf,
    /// Signalled by the crashed thread when the exception pointers are in place.
    request: Event,
    /// Signalled by the writer when the dump and report are on disk.
    done: Event,
    pointers: AtomicUsize,
    thread_id: AtomicU32,
}

impl Writer {
    fn spawn(dir: PathBuf) -> io::Result<Self> {
        let inner = Arc::new(WriterInner {
            dir,
            request: Event::create()?,
            done: Event::create()?,
            pointers: AtomicUsize::new(0),
            thread_id: AtomicU32::new(0),
        });
        let writer = Arc::clone(&inner);
        std::thread::Builder::new()
            .name("crash-dump-writer".to_owned())
            .spawn(move || writer.run())?;
        Ok(Self { inner })
    }

    fn request(&self, pointers: *const EXCEPTION_POINTERS, thread_id: u32) {
        self.inner
            .pointers
            .store(pointers as usize, Ordering::SeqCst);
        self.inner.thread_id.store(thread_id, Ordering::SeqCst);
        self.inner.request.set();
    }

    fn wait(&self) -> bool {
        self.inner.done.wait(WRITER_TIMEOUT_MS)
    }
}

impl WriterInner {
    fn run(self: Arc<Self>) {
        if !self.request.wait(INFINITE) {
            return;
        }
        let pointers = self.pointers.load(Ordering::SeqCst) as *const EXCEPTION_POINTERS;
        let thread_id = self.thread_id.load(Ordering::SeqCst);
        let _ = write_crash(&self.dir, pointers, thread_id);
        self.done.set();
    }
}

/// A Windows event that any thread of the process may signal or wait on.
struct Event(HANDLE);

// SAFETY: Windows event handles are process-wide objects and may be used from any thread.
unsafe impl Send for Event {}
unsafe impl Sync for Event {}

impl Event {
    fn create() -> io::Result<Self> {
        // SAFETY: default security, auto-reset, initially unsignalled and unnamed.
        let handle = unsafe { CreateEventW(std::ptr::null(), 0, 0, std::ptr::null()) };
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(handle))
    }

    fn set(&self) {
        // SAFETY: the handle is open for the lifetime of `self`.
        unsafe { SetEvent(self.0) };
    }

    fn wait(&self, millis: u32) -> bool {
        // SAFETY: the handle is open for the lifetime of `self`.
        unsafe { WaitForSingleObject(self.0, millis) == WAIT_OBJECT_0 }
    }
}

impl Drop for Event {
    fn drop(&mut self) {
        // SAFETY: the handle was created by `CreateEventW` and is owned by this value.
        unsafe { CloseHandle(self.0) };
    }
}

/// One write per process: a crash inside the writer must not start a second write.
struct WriteGate(AtomicBool);

impl WriteGate {
    const fn new() -> Self {
        Self(AtomicBool::new(false))
    }

    /// Claims the right to write; `false` means a write is already in progress.
    fn begin(&self) -> bool {
        !self.0.swap(true, Ordering::SeqCst)
    }
}

fn write_crash(dir: &Path, pointers: *const EXCEPTION_POINTERS, thread_id: u32) -> io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let time = SystemTime::now();
    write_dump(&dir.join(dump_name(time)), pointers, thread_id)?;
    std::fs::write(dir.join(report_name(time)), report(pointers, thread_id))?;
    prune(dir, KEEP_CRASHES)
}

fn write_dump(path: &Path, pointers: *const EXCEPTION_POINTERS, thread_id: u32) -> io::Result<()> {
    // DbgHelp owns process-wide state, so one dump of this process runs at a time.
    let _guard = DUMP_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let file = std::fs::File::create(path)?;
    let info = MINIDUMP_EXCEPTION_INFORMATION {
        // The crashed thread, not the writer that calls `MiniDumpWriteDump`.
        ThreadId: thread_id,
        ExceptionPointers: pointers.cast_mut(),
        ClientPointers: 0,
    };
    let info = if pointers.is_null() {
        std::ptr::null()
    } else {
        &raw const info
    };
    let kind: MINIDUMP_TYPE = MiniDumpNormal | MiniDumpWithDataSegs | MiniDumpWithThreadInfo;
    // SAFETY: the process and thread pseudohandles belong to this process, the file handle
    // is open for the call, and `info` outlives it.
    let done = unsafe {
        MiniDumpWriteDump(
            GetCurrentProcess(),
            GetCurrentProcessId(),
            file.as_raw_handle() as HANDLE,
            kind,
            info,
            std::ptr::null(),
            std::ptr::null(),
        )
    };
    if done == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// The exception code, address, module and thread, which is enough to find the dump and
/// read the first frame without a debugger.
fn report(pointers: *const EXCEPTION_POINTERS, thread_id: u32) -> String {
    // SAFETY: the system passes a valid exception record to the filter.
    let exception = unsafe { exception(pointers) };
    report_text(exception, thread_id)
}

fn report_text(exception: Option<(u32, usize)>, thread_id: u32) -> String {
    let body = match exception {
        Some((code, address)) => {
            let module = module_of(address).map_or_else(
                || "unknown".to_owned(),
                |(path, offset)| format!("{path}+0x{offset:x}"),
            );
            format!(
                "{}\naddress: 0x{address:016X}\nmodule: {module}\nthread: {thread_id}\n",
                exception_line(code)
            )
        }
        None => {
            let module = module_path(std::ptr::null_mut()).unwrap_or_else(|| "unknown".to_owned());
            format!(
                "exception: abort (SIGABRT)\naddress: 0x0000000000000000\nmodule: {module}\nthread: {thread_id}\n"
            )
        }
    };
    format!("{}{body}", header())
}

/// The three lines that name the build, so a user's report can be matched with its
/// symbols even when the binary on disk is newer.
fn header() -> String {
    let (version, commit) = IDENTITY.get().map_or(("unknown", "unknown"), |identity| {
        (identity.version.as_str(), identity.commit.as_str())
    });
    format!(
        "version: {version}\ncommit: {commit}\nos: {}\n",
        crate::os::version()
    )
}

fn exception_line(code: u32) -> String {
    match exception_name(code) {
        Some(name) => format!("exception: {name} (0x{code:08X})"),
        None => format!("exception: 0x{code:08X}"),
    }
}

fn exception_name(code: u32) -> Option<&'static str> {
    if code == EXCEPTION_STACK_OVERFLOW as u32 {
        Some("stack overflow")
    } else {
        None
    }
}

unsafe fn exception(pointers: *const EXCEPTION_POINTERS) -> Option<(u32, usize)> {
    if pointers.is_null() {
        return None;
    }
    // SAFETY: the system passes a valid exception record to the filter.
    let record = unsafe { (*pointers).ExceptionRecord };
    if record.is_null() {
        return None;
    }
    // SAFETY: the record belongs to the exception and is readable for the call.
    let record = unsafe { &*record };
    Some((
        record.ExceptionCode as u32,
        record.ExceptionAddress as usize,
    ))
}

/// The module that owns an address, plus the offset inside it.
pub fn module_of(address: usize) -> Option<(String, usize)> {
    let mut module: HMODULE = std::ptr::null_mut();
    let flags =
        GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT;
    // SAFETY: an address inside a module is a valid name, and the returned handle is only
    // read before it is dropped.
    let done = unsafe { GetModuleHandleExW(flags, address as *const u16, &mut module) };
    if done == 0 {
        return None;
    }
    let path = module_path(module)?;
    Some((path, address.saturating_sub(module as usize)))
}

fn module_path(module: HMODULE) -> Option<String> {
    let mut buffer = [0u16; 512];
    // SAFETY: the buffer and its length are a valid UTF-16 destination.
    let len = unsafe { GetModuleFileNameW(module, buffer.as_mut_ptr(), buffer.len() as u32) };
    if len == 0 || len as usize >= buffer.len() {
        return None;
    }
    Some(String::from_utf16_lossy(&buffer[..len as usize]))
}

/// A name without colons, so that it works on Windows too.
fn dump_name(time: SystemTime) -> String {
    format!("{}.dmp", stem(time))
}

fn report_name(time: SystemTime) -> String {
    format!("{}.log", stem(time))
}

fn stem(time: SystemTime) -> String {
    let millis = time
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis());
    format!("crash-{millis}")
}

/// Keeps the newest `keep` crashes and removes the rest, together with their reports.
/// Files that this module did not name are untouched.
fn prune(dir: &Path, keep: usize) -> io::Result<()> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    let mut crashes: Vec<(u128, PathBuf)> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !entry.file_type().is_ok_and(|kind| kind.is_file()) {
            continue;
        }
        let Some(millis) = crash_millis(&entry.file_name()) else {
            continue;
        };
        crashes.push((millis, path));
    }
    crashes.sort_by_key(|crash| std::cmp::Reverse(crash.0));
    let mut kept: Vec<u128> = Vec::new();
    for (millis, _) in &crashes {
        if kept.contains(millis) {
            continue;
        }
        if kept.len() == keep {
            break;
        }
        kept.push(*millis);
    }
    for (millis, path) in crashes {
        if !kept.contains(&millis) {
            let _ = std::fs::remove_file(path);
        }
    }
    Ok(())
}

/// The milliseconds of a `crash-<millis>.dmp` or `crash-<millis>.log` name.
fn crash_millis(name: &std::ffi::OsStr) -> Option<u128> {
    let name = name.to_str()?;
    let rest = name.strip_prefix("crash-")?;
    let (millis, extension) = rest.split_once('.')?;
    if extension != "dmp" && extension != "log" {
        return None;
    }
    millis.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::{
        KEEP_CRASHES, WriteGate, Writer, dump_name, prune, report_name, report_text, write_crash,
        write_dump,
    };
    use std::path::PathBuf;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};
    use windows_sys::Win32::System::Threading::GetCurrentThreadId;

    fn temp_dir(label: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "platform-crash-{}-{label}-{unique}",
            std::process::id()
        ))
    }

    #[test]
    fn the_names_are_a_time_and_windows_safe() {
        let time = UNIX_EPOCH + Duration::from_millis(1_765_000_123_456);
        assert_eq!(dump_name(time), "crash-1765000123456.dmp");
        assert_eq!(report_name(time), "crash-1765000123456.log");
        assert!(!dump_name(time).contains(':'));
    }

    #[test]
    fn only_the_newest_crashes_stay() {
        let dir = temp_dir("prune");
        std::fs::create_dir_all(&dir).unwrap();
        for millis in 1..=12u128 {
            std::fs::write(dir.join(format!("crash-{millis}.dmp")), b"dump").unwrap();
            std::fs::write(dir.join(format!("crash-{millis}.log")), b"text").unwrap();
        }
        std::fs::write(dir.join("panic-1.log"), b"keep").unwrap();
        prune(&dir, KEEP_CRASHES).unwrap();
        for millis in 1..=2u128 {
            assert!(!dir.join(format!("crash-{millis}.dmp")).exists());
            assert!(!dir.join(format!("crash-{millis}.log")).exists());
        }
        for millis in 3..=12u128 {
            assert!(dir.join(format!("crash-{millis}.dmp")).exists());
            assert!(dir.join(format!("crash-{millis}.log")).exists());
        }
        assert!(dir.join("panic-1.log").exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_dump_of_this_process_is_not_empty() {
        let _guard = crate::WIN32_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let dir = temp_dir("dump");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("current.dmp");
        let thread_id = unsafe { GetCurrentThreadId() };
        write_dump(&path, std::ptr::null(), thread_id).unwrap();
        let size = std::fs::metadata(&path).unwrap().len();
        assert!(size > 0, "the dump is {size} bytes");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_writer_thread_writes_a_dump_of_this_process() {
        let _guard = crate::WIN32_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let dir = temp_dir("writer");
        std::fs::create_dir_all(&dir).unwrap();
        let writer = Writer::spawn(dir.clone()).unwrap();
        let thread_id = unsafe { GetCurrentThreadId() };
        writer.request(std::ptr::null(), thread_id);
        assert!(writer.wait(), "the writer thread did not finish");
        let dump = std::fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| path.extension().is_some_and(|extension| extension == "dmp"))
            .expect("a dump was written");
        let size = std::fs::metadata(dump).unwrap().len();
        assert!(size > 0, "the dump is {size} bytes");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_second_write_is_refused_while_the_first_is_running() {
        let gate = WriteGate::new();
        assert!(gate.begin());
        assert!(!gate.begin());
    }

    #[test]
    fn a_stack_overflow_is_named_in_the_report() {
        let text = report_text(Some((0xC000_00FD, 0)), 7);
        assert!(text.contains("exception: stack overflow"), "{text}");
        assert!(text.contains("0xC00000FD"), "{text}");
        assert!(text.contains("thread: 7"), "{text}");
    }

    #[test]
    fn the_report_starts_with_the_version_commit_and_os() {
        let text = report_text(None, 1);
        let mut lines = text.lines();
        assert!(
            lines
                .next()
                .is_some_and(|line| line.starts_with("version: "))
        );
        assert!(
            lines
                .next()
                .is_some_and(|line| line.starts_with("commit: "))
        );
        assert!(lines.next().is_some_and(|line| line.starts_with("os: ")));
        assert!(text.contains("exception: abort"), "{text}");
    }

    #[test]
    fn a_crash_writes_a_dump_and_a_report() {
        let _guard = crate::WIN32_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let dir = temp_dir("crash");
        let thread_id = unsafe { GetCurrentThreadId() };
        write_crash(&dir, std::ptr::null(), thread_id).unwrap();
        let names: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(names.iter().any(|name| name.ends_with(".dmp")));
        assert!(names.iter().any(|name| name.ends_with(".log")));
        std::fs::remove_dir_all(&dir).ok();
    }
}

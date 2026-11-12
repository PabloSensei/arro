//! A log for a crash that never reaches the panic hook: a segmentation fault, a bus
//! error or an abort. The panic hook of the application writes its own file next to it.
//!
//! A signal handler may call only a few functions, so the log of a signal is a fixed
//! line written with `open`, `write` and `close`, and then the signal carries on as
//! before, which keeps core dumps working.

use std::ffi::{CStr, CString, c_int, c_void};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// How many crash logs stay on disk.
const KEEP_CRASHES: usize = 10;

/// The file a signal handler appends to, prepared at install time.
static SIGNAL_LOG: OnceLock<CString> = OnceLock::new();

/// The signals that may end a process without a panic.
const SIGNALS: &[(c_int, &str)] = &[
    (libc::SIGSEGV, "segmentation fault"),
    (libc::SIGBUS, "bus error"),
    (libc::SIGILL, "illegal instruction"),
    (libc::SIGFPE, "arithmetic exception"),
    (libc::SIGABRT, "abort"),
];

/// Writes a line for every fatal signal into `dir`.
pub fn install(dir: PathBuf, _version: String, _commit: String) -> io::Result<()> {
    std::fs::create_dir_all(&dir)?;
    let _ = prune(&dir, KEEP_CRASHES);
    let log = dir.join("crash-signals.log");
    let log = CString::new(log.as_os_str().as_encoded_bytes()).map_err(|_| {
        io::Error::new(io::ErrorKind::InvalidInput, "the crash log has a zero byte")
    })?;
    let _ = SIGNAL_LOG.set(log);

    for &(signal, _) in SIGNALS {
        // SAFETY: the handler is a function that lives for the whole process, and an
        // all-zero `sigaction` is a plain value with the default flags.
        unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = handle_signal as *const () as usize;
            action.sa_flags = libc::SA_SIGINFO;
            libc::sigemptyset(&mut action.sa_mask);
            if libc::sigaction(signal, &action, std::ptr::null_mut()) != 0 {
                return Err(io::Error::last_os_error());
            }
        }
    }
    Ok(())
}

/// The name and the offset of the module that holds `address`, for the backtrace of a
/// panic log. A statically linked binary has no module and answers `None`.
pub fn module_of(address: usize) -> Option<(String, usize)> {
    // SAFETY: `dladdr` fills the structure for any address and does not keep it.
    let mut info: libc::Dl_info = unsafe { std::mem::zeroed() };
    // SAFETY: the address is a plain value and the structure is writable.
    if unsafe { libc::dladdr(address as *const c_void, &mut info) } == 0 || info.dli_fname.is_null()
    {
        return None;
    }
    // SAFETY: a non-null name from `dladdr` is a zero-terminated string.
    let path = unsafe { CStr::from_ptr(info.dli_fname) }
        .to_string_lossy()
        .into_owned();
    Some((path, address.saturating_sub(info.dli_fbase as usize)))
}

unsafe extern "C" fn handle_signal(signal: c_int, _info: *mut c_void, _context: *mut c_void) {
    if let Some(path) = SIGNAL_LOG.get() {
        // SAFETY: plain calls on a path prepared before any crash; everything here is
        // async-signal-safe.
        unsafe {
            let descriptor = libc::open(
                path.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_APPEND,
                0o600,
            );
            if descriptor >= 0 {
                let mut line = [0u8; 64];
                let prefix = b"crash: ";
                line[..prefix.len()].copy_from_slice(prefix);
                let mut length = prefix.len();
                let name = SIGNALS
                    .iter()
                    .find(|(number, _)| *number == signal)
                    .map_or("signal", |(_, name)| name);
                line[length..length + name.len()].copy_from_slice(name.as_bytes());
                length += name.len();
                line[length..length + 2].copy_from_slice(b" (");
                length += 2;
                let mut digits = [0u8; 3];
                let mut number = signal;
                let mut count = 0;
                while number > 0 && count < digits.len() {
                    digits[count] = b'0' + (number % 10) as u8;
                    number /= 10;
                    count += 1;
                }
                if count == 0 {
                    digits[0] = b'0';
                    count = 1;
                }
                for digit in digits[..count].iter().rev() {
                    line[length] = *digit;
                    length += 1;
                }
                line[length..length + 2].copy_from_slice(b")\n");
                length += 2;
                libc::write(descriptor, line.as_ptr().cast(), length);
                libc::close(descriptor);
            }
            // The default action writes the core dump and ends the process.
            libc::signal(signal, libc::SIG_DFL);
            libc::raise(signal);
        }
    }
}

/// Keeps the newest `keep` crash logs, by name, which sorts by time.
fn prune(dir: &Path, keep: usize) -> io::Result<()> {
    let mut logs: Vec<PathBuf> = std::fs::read_dir(dir)?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("crash-"))
        })
        .collect();
    if logs.len() <= keep {
        return Ok(());
    }
    logs.sort();
    for path in &logs[..logs.len() - keep] {
        let _ = std::fs::remove_file(path);
    }
    Ok(())
}

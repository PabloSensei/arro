//! The instance endpoint on Windows: a named pipe of the state folder.
//!
//! The pipe is opened for overlapped input and output, so every read has a limit: a
//! client that connects and says nothing lets the listener go on to the next one. A
//! second start also stops waiting for the acknowledgement of a program that stopped
//! answering.

use std::fs::File;
use std::io::{self, BufReader, Read, Write};
use std::os::windows::io::{FromRawHandle, RawHandle};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_ACCESS_DENIED, ERROR_FILE_NOT_FOUND, ERROR_IO_PENDING, ERROR_PIPE_BUSY,
    ERROR_PIPE_CONNECTED, GetLastError, HANDLE, INVALID_HANDLE_VALUE, LocalFree, WAIT_OBJECT_0,
    WAIT_TIMEOUT,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::{
    GetTokenInformation, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
    TokenUser,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, OPEN_EXISTING,
    PIPE_ACCESS_DUPLEX, ReadFile, WriteFile,
};
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, NAMED_PIPE_MODE, PIPE_READMODE_BYTE,
    PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT, WaitNamedPipeW,
};
use windows_sys::Win32::System::Threading::{
    CreateEventW, GetCurrentProcess, OpenProcessToken, WaitForSingleObject,
};

use super::{Request, read_ack, serve, write_request};

/// How long a second start waits for the acknowledgement, and how long the listener
/// waits for the request of a connection it accepted.
const WAIT: Duration = Duration::from_secs(5);
const REQUEST_WAIT: Duration = Duration::from_secs(2);
/// How long a second start waits for a busy instance to take it.
const CONNECT_WAIT: Duration = Duration::from_secs(10);
const BUFFER: u32 = 4096;
/// Bytes both ways, a client waited for, and no client from another machine.
const PIPE_MODE: NAMED_PIPE_MODE =
    PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS;

/// The listener of a state folder. Dropping it stops the thread and closes the pipe.
pub struct Instance {
    name: Vec<u16>,
    stop: Arc<AtomicBool>,
    finished: Receiver<()>,
    thread: Option<JoinHandle<()>>,
}

impl Instance {
    /// Listens on the pipe of `state_dir` and passes every request to `on_request`.
    pub fn listen<F>(state_dir: &Path, mut on_request: F) -> io::Result<Self>
    where
        F: FnMut(Request) + Send + 'static,
    {
        std::fs::create_dir_all(state_dir)?;
        let name = pipe_name(state_dir)?;
        // Only the account of this program may open the pipe. The descriptor is read when
        // the pipe is made, so it lives until then.
        let security = PipeSecurity::for_current_user()?;
        let attributes = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: security.0,
            bInheritHandle: 0,
        };
        // The first instance of the name belongs to this program; a second one of a
        // running program is refused, and the caller sends its request instead.
        let handle = unsafe {
            CreateNamedPipeW(
                name.as_ptr(),
                PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED | FILE_FLAG_FIRST_PIPE_INSTANCE,
                PIPE_MODE,
                1,
                BUFFER,
                BUFFER,
                0,
                &attributes,
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            let error = unsafe { GetLastError() };
            if error == ERROR_ACCESS_DENIED {
                return Err(io::Error::new(
                    io::ErrorKind::AddrInUse,
                    "another instance is listening",
                ));
            }
            return Err(io::Error::from_raw_os_error(error as i32));
        }
        // SAFETY: the handle is a pipe of this process and the File closes it once.
        let pipe = unsafe { File::from_raw_handle(handle as RawHandle) };
        let stop = Arc::new(AtomicBool::new(false));
        let (sender, finished) = channel();
        let listener_stop = Arc::clone(&stop);
        let thread = thread::Builder::new()
            .name("instance".into())
            .spawn(move || {
                while !listener_stop.load(Ordering::Relaxed) {
                    // SAFETY: the handle stays open for the whole loop.
                    match accept(raw(&pipe)) {
                        Ok(()) => {}
                        Err(error) => {
                            if listener_stop.load(Ordering::Relaxed) {
                                break;
                            }
                            eprintln!("the instance does not take a connection: {error}");
                            break;
                        }
                    }
                    // A malformed or abandoned connection only ends itself.
                    {
                        let mut reader = BufReader::new(PipeStream {
                            handle: raw(&pipe),
                            timeout: REQUEST_WAIT,
                        });
                        let mut writer = PipeStream {
                            handle: raw(&pipe),
                            timeout: WAIT,
                        };
                        let _ = serve(&mut reader, &mut writer, &mut on_request);
                    }
                    // SAFETY: the handle stays open for the whole loop.
                    unsafe { DisconnectNamedPipe(raw(&pipe)) };
                }
                sender.send(()).ok();
            })?;
        Ok(Self {
            name,
            stop,
            finished,
            thread: Some(thread),
        })
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // The listener waits in `ConnectNamedPipe`: a connection of our own ends that
        // wait. A busy listener has a client of its own; it lets it go after the read
        // limit, and the retry of `connect` reaches it then.
        let _ = connect(&self.name);
        if self.finished.recv_timeout(WAIT).is_ok() {
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        } else {
            // The listener did not stop in time: its thread ends with the program.
            self.thread.take();
        }
    }
}

fn raw(pipe: &File) -> HANDLE {
    std::os::windows::io::AsRawHandle::as_raw_handle(pipe) as HANDLE
}

/// The name of the pipe of a state folder: the folder itself is not a legal pipe name,
/// so the name is a hash of its absolute path. Windows compares paths without case. The
/// name carries the SID of the account too, so the pipe of another account has another name.
fn pipe_name(state_dir: &Path) -> io::Result<Vec<u16>> {
    let full = std::path::absolute(state_dir).unwrap_or_else(|_| PathBuf::from(state_dir));
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in full.to_string_lossy().to_lowercase().bytes() {
        hash = (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3);
    }
    let sid = current_user_sid()?;
    let mut name: Vec<u16> = format!(r"\\.\pipe\arro-{sid}-{hash:016x}")
        .encode_utf16()
        .collect();
    name.push(0);
    Ok(name)
}

/// The SID of the account that runs this program, as text, such as `S-1-5-21-...`.
fn current_user_sid() -> io::Result<String> {
    let mut token: HANDLE = std::ptr::null_mut();
    // SAFETY: the pseudo handle of this process needs no closing; `token` is closed below.
    let opened = unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) };
    if opened == 0 {
        return Err(io::Error::last_os_error());
    }
    let user = token_user(token);
    // SAFETY: the token was opened above and nothing uses it after this.
    unsafe { CloseHandle(token) };
    let user = user?;
    // SAFETY: the buffer holds the `TOKEN_USER` the system wrote, and `user` lives while the
    // SID is read from it.
    let sid = unsafe { (*user.as_ptr().cast::<TOKEN_USER>()).User.Sid };
    let mut text = std::ptr::null_mut();
    // SAFETY: `sid` points into `user`; the text the system allocates is freed below.
    if unsafe { ConvertSidToStringSidW(sid, &mut text) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the system wrote a NUL-terminated string, which stays valid until freed.
    let value = unsafe { wide_string(text) };
    // SAFETY: `text` came from the conversion above and is freed once.
    unsafe { LocalFree(text.cast()) };
    Ok(value)
}

/// The `TOKEN_USER` of a token. The buffer is of 8-byte words, so the structure is aligned.
fn token_user(token: HANDLE) -> io::Result<Vec<u64>> {
    let mut needed = 0;
    // SAFETY: a null buffer of length zero asks only for the size.
    unsafe { GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut needed) };
    let mut buffer = vec![0_u64; (needed as usize).div_ceil(std::mem::size_of::<u64>())];
    // SAFETY: the buffer is as large as the system asked for.
    let read = unsafe {
        GetTokenInformation(
            token,
            TokenUser,
            buffer.as_mut_ptr().cast(),
            needed,
            &mut needed,
        )
    };
    if read == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(buffer)
}

/// The text of a NUL-terminated UTF-16 string.
///
/// # Safety
///
/// `text` points to a NUL-terminated string that stays valid for the call.
unsafe fn wide_string(text: *const u16) -> String {
    let mut length = 0;
    // SAFETY: the loop stops at the terminating NUL, which lies inside the string.
    while unsafe { *text.add(length) } != 0 {
        length += 1;
    }
    // SAFETY: `length` units up to the NUL are inside the string.
    let units = unsafe { std::slice::from_raw_parts(text, length) };
    String::from_utf16_lossy(units)
}

/// The security descriptor in SDDL: a protected access list (`P`, nothing inherited from
/// the folder) with one entry that allows every access (`GA`) to `sid` and to no other.
fn pipe_sddl(sid: &str) -> String {
    format!("D:P(A;;GA;;;{sid})")
}

/// A security descriptor of the pipe, given by the system. Freed when dropped.
struct PipeSecurity(PSECURITY_DESCRIPTOR);

impl PipeSecurity {
    /// The descriptor that lets only the account of this program open the pipe.
    fn for_current_user() -> io::Result<Self> {
        let text: Vec<u16> = pipe_sddl(&current_user_sid()?)
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let mut descriptor = std::ptr::null_mut();
        // SAFETY: `text` is NUL-terminated and lives for the call; the system allocates the
        // descriptor, which `Drop` frees.
        let converted = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                text.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                std::ptr::null_mut(),
            )
        };
        if converted == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(descriptor))
    }
}

impl Drop for PipeSecurity {
    fn drop(&mut self) {
        // SAFETY: the descriptor came from the conversion in `for_current_user` and is freed once.
        unsafe { LocalFree(self.0) };
    }
}

/// One overlapped operation with the event that tells when it ends. The kernel may still
/// write into the caller's buffer until the operation completes, so every operation is
/// waited for even after it was cancelled.
struct Operation {
    overlapped: OVERLAPPED,
    event: HANDLE,
}

impl Operation {
    fn new() -> io::Result<Self> {
        let event = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
        if event.is_null() {
            return Err(io::Error::last_os_error());
        }
        let overlapped = OVERLAPPED {
            hEvent: event,
            ..OVERLAPPED::default()
        };
        Ok(Self { overlapped, event })
    }

    /// True when the operation ended in time; `None` waits without a limit.
    fn wait(&self, timeout: Option<Duration>) -> io::Result<bool> {
        let millis = timeout.map_or(u32::MAX, |timeout| {
            timeout.as_millis().min(u128::from(u32::MAX - 1)) as u32
        });
        match unsafe { WaitForSingleObject(self.event, millis) } {
            WAIT_OBJECT_0 => Ok(true),
            WAIT_TIMEOUT => Ok(false),
            _ => Err(io::Error::last_os_error()),
        }
    }

    /// The bytes of an operation that ended, or the error that ended it.
    fn bytes(&mut self, handle: HANDLE) -> io::Result<usize> {
        let mut count = 0u32;
        let ok = unsafe { GetOverlappedResult(handle, &self.overlapped, &mut count, 1) };
        if ok == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(count as usize)
        }
    }

    fn cancel(&mut self, handle: HANDLE) {
        unsafe { CancelIoEx(handle, &self.overlapped) };
    }
}

impl Drop for Operation {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.event) };
    }
}

/// The outcome of an overlapped operation, with the limit of its wait. A cancelled read
/// is waited for before the buffer of the caller leaves the stack.
fn finished(
    started: i32,
    operation: &mut Operation,
    handle: HANDLE,
    timeout: Duration,
) -> io::Result<usize> {
    if started == 0 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(ERROR_IO_PENDING as i32) {
            return Err(error);
        }
        if !operation.wait(Some(timeout))? {
            operation.cancel(handle);
            let _ = operation.bytes(handle);
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "the other side of the pipe did not answer in time",
            ));
        }
    }
    operation.bytes(handle)
}

fn read(handle: HANDLE, buffer: &mut [u8], timeout: Duration) -> io::Result<usize> {
    if buffer.is_empty() {
        return Ok(0);
    }
    let mut operation = Operation::new()?;
    let started = unsafe {
        ReadFile(
            handle,
            buffer.as_mut_ptr(),
            buffer.len() as u32,
            std::ptr::null_mut(),
            &mut operation.overlapped,
        )
    };
    finished(started, &mut operation, handle, timeout)
}

fn write(handle: HANDLE, buffer: &[u8], timeout: Duration) -> io::Result<usize> {
    if buffer.is_empty() {
        return Ok(0);
    }
    let mut operation = Operation::new()?;
    let started = unsafe {
        WriteFile(
            handle,
            buffer.as_ptr(),
            buffer.len() as u32,
            std::ptr::null_mut(),
            &mut operation.overlapped,
        )
    };
    finished(started, &mut operation, handle, timeout)
}

/// One end of the pipe as a reader and a writer with a limit on every operation.
struct PipeStream {
    handle: HANDLE,
    timeout: Duration,
}

impl Read for PipeStream {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        read(self.handle, buffer, self.timeout)
    }
}

impl Write for PipeStream {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        write(self.handle, buffer, self.timeout)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Waits for a client to connect. The wait has no limit: the end of the program connects
/// to its own pipe and ends it.
fn accept(handle: HANDLE) -> io::Result<()> {
    let mut operation = Operation::new()?;
    let started = unsafe { ConnectNamedPipe(handle, &mut operation.overlapped) };
    if started != 0 {
        return Ok(());
    }
    let error = unsafe { GetLastError() };
    match error {
        // A client that connected between the create and the connect.
        ERROR_PIPE_CONNECTED => Ok(()),
        ERROR_IO_PENDING => {
            operation.wait(None)?;
            operation.bytes(handle).map(|_| ())
        }
        _ => Err(io::Error::from_raw_os_error(error as i32)),
    }
}

/// Opens the client end of the pipe. `Ok(None)` means no program listens.
fn connect(name: &[u16]) -> io::Result<Option<File>> {
    let deadline = Instant::now() + CONNECT_WAIT;
    loop {
        let handle = unsafe {
            CreateFileW(
                name.as_ptr(),
                windows_sys::Win32::Foundation::GENERIC_READ
                    | windows_sys::Win32::Foundation::GENERIC_WRITE,
                0,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED,
                std::ptr::null_mut(),
            )
        };
        if handle != INVALID_HANDLE_VALUE {
            // SAFETY: the handle is the client end of the pipe and the File closes it.
            return Ok(Some(unsafe { File::from_raw_handle(handle as RawHandle) }));
        }
        let error = unsafe { GetLastError() };
        if error == ERROR_FILE_NOT_FOUND {
            return Ok(None);
        }
        if error != ERROR_PIPE_BUSY {
            return Err(io::Error::from_raw_os_error(error as i32));
        }
        // The instance is serving another start: wait for it to listen again.
        let now = Instant::now();
        if now >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "the instance did not listen again",
            ));
        }
        let millis = (deadline - now).as_millis().min(u128::from(u32::MAX)) as u32;
        if unsafe { WaitNamedPipeW(name.as_ptr(), millis.max(1)) } == 0 {
            let error = unsafe { GetLastError() };
            if error == ERROR_FILE_NOT_FOUND {
                return Ok(None);
            }
        }
    }
}

/// Passes `request` to the program listening in `state_dir`. `Ok(true)` means the
/// program took it and answered; `Ok(false)` means no program listens.
pub fn send(state_dir: &Path, request: &Request) -> io::Result<bool> {
    let name = pipe_name(state_dir)?;
    let Some(pipe) = connect(&name)? else {
        return Ok(false);
    };
    let handle = raw(&pipe);
    write_request(
        &mut PipeStream {
            handle,
            timeout: WAIT,
        },
        request,
    )?;
    let mut reader = BufReader::new(PipeStream {
        handle,
        timeout: WAIT,
    });
    read_ack(&mut reader)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    use test_support::TempDir;

    /// A client that connected and says nothing holds the only instance of the pipe.
    /// After the read limit the listener lets it go, and the next start is served.
    #[test]
    fn a_silent_client_does_not_keep_the_listener() {
        let folder = TempDir::new("i-silent");
        let (sender, receiver) = mpsc::channel();
        let instance = Instance::listen(folder.path(), move |request| {
            sender.send(request).ok();
        })
        .unwrap();
        let silent = connect(&pipe_name(folder.path()).unwrap())
            .unwrap()
            .expect("the instance listens");

        assert!(
            send(folder.path(), &Request::Focus).unwrap(),
            "the next start reached the listener"
        );
        assert_eq!(
            receiver.recv_timeout(Duration::from_secs(30)).unwrap(),
            Request::Focus
        );
        drop(silent);
        drop(instance);
    }

    /// The name of the pipe is the account's own: the SID of the running account is in it.
    #[test]
    fn the_pipe_name_carries_the_account_sid() {
        let folder = TempDir::new("i-sid");
        let sid = current_user_sid().unwrap();
        assert!(sid.starts_with("S-1-"), "{sid}");
        let name = String::from_utf16_lossy(&pipe_name(folder.path()).unwrap());
        assert!(name.contains(&sid), "{name}");
    }

    /// The pipe refuses clients of other machines, on top of the access list.
    #[test]
    fn the_pipe_refuses_remote_clients() {
        assert_eq!(
            PIPE_MODE & PIPE_REJECT_REMOTE_CLIENTS,
            PIPE_REJECT_REMOTE_CLIENTS
        );
    }

    #[test]
    fn the_access_list_names_the_account_alone() {
        assert_eq!(
            pipe_sddl("S-1-5-21-1-2-3-1001"),
            "D:P(A;;GA;;;S-1-5-21-1-2-3-1001)"
        );
        assert!(PipeSecurity::for_current_user().is_ok());
    }
}

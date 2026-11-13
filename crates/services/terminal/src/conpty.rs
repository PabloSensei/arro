//! The pseudoconsole and its process. `conpty.dll` with its `OpenConsole.exe` comes from NuGet,
//! because the conhost built into Windows lags behind. The system API stays as the fallback, so a
//! checkout without the vendored files still works.

use std::env;
use std::ffi::{OsStr, c_void};
use std::fs;
use std::fs::File;
use std::io;
use std::mem::{size_of, zeroed};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};
use std::sync::{Mutex, OnceLock};
use std::thread;

use platform::{Job, allow_ctrl_c_in_children};
use windows_sys::Win32::Foundation::{GetLastError, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0};
use windows_sys::Win32::Security::Cryptography::{
    BCRYPT_ALG_HANDLE, BCRYPT_SHA256_ALGORITHM, BCryptCloseAlgorithmProvider, BCryptHash,
    BCryptOpenAlgorithmProvider,
};
use windows_sys::Win32::System::Console::{
    COORD, ClosePseudoConsole, CreatePseudoConsole, HPCON, ResizePseudoConsole,
};
use windows_sys::Win32::System::LibraryLoader::{
    GetProcAddress, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW,
};
use windows_sys::Win32::System::Pipes::CreatePipe;
use windows_sys::Win32::System::Threading::{
    CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessW, DeleteProcThreadAttributeList,
    EXTENDED_STARTUPINFO_PRESENT, GetExitCodeProcess, INFINITE, InitializeProcThreadAttributeList,
    PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE, PROCESS_INFORMATION, ResumeThread, STARTF_USESTDHANDLES,
    STARTUPINFOEXW, TerminateProcess, UpdateProcThreadAttribute, WaitForSingleObject,
};

use crate::session::SpawnSpec;

/// Keeps grapheme handling in conhost aligned with the per-char widths of `Term`.
const GLYPH_WIDTH_WCSWIDTH: u32 = 0x10;

const DIRECTORY_VARIABLE: &str = "IDE_CONPTY_DIR";

/// The pinned hashes, filled in by `cargo run -p xtask -- fetch-conpty`.
const PINS: &str = include_str!("../conpty.lock");
const SHA256_HEX: usize = 64;

type RawFn = unsafe extern "system" fn() -> isize;
type CreateFn = unsafe extern "system" fn(COORD, HANDLE, HANDLE, u32, *mut HPCON) -> i32;
type ResizeFn = unsafe extern "system" fn(HPCON, COORD) -> i32;
type CloseFn = unsafe extern "system" fn(HPCON);

/// Which conhost serves the session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    Vendored,
    System,
}

#[derive(Clone, Copy)]
struct Api {
    backend: Backend,
    create: CreateFn,
    resize: ResizeFn,
    close: CloseFn,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Pins {
    conpty_dll: Option<String>,
    open_console_exe: Option<String>,
}

/// Reads the `key = value` lines of `conpty.lock`; comments start with `#`.
fn parse_pins(text: &str) -> Pins {
    let mut pins = Pins::default();
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or_default().trim();
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = pinned_hash(value);
        match key.trim() {
            "conpty_dll_sha256" => pins.conpty_dll = value,
            "open_console_exe_sha256" => pins.open_console_exe = value,
            _ => {}
        }
    }
    pins
}

/// A hash counts only as 64 hex digits. Anything else is treated as not pinned.
fn pinned_hash(value: &str) -> Option<String> {
    let value = value.trim().trim_matches(['"', '\'']).to_ascii_lowercase();
    (value.len() == SHA256_HEX && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then_some(value)
}

fn hex(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write;
        let _ = write!(text, "{byte:02x}");
    }
    text
}

/// One hash through CNG. The files are small, and this runs once per process.
fn sha256(data: &[u8]) -> Option<String> {
    let mut algorithm: BCRYPT_ALG_HANDLE = null_mut();
    let opened =
        unsafe { BCryptOpenAlgorithmProvider(&mut algorithm, BCRYPT_SHA256_ALGORITHM, null(), 0) };
    if opened < 0 {
        return None;
    }
    let mut digest = [0u8; 32];
    let hashed = unsafe {
        let status = BCryptHash(
            algorithm,
            null(),
            0,
            data.as_ptr(),
            data.len() as u32,
            digest.as_mut_ptr(),
            digest.len() as u32,
        );
        BCryptCloseAlgorithmProvider(algorithm, 0);
        status
    };
    (hashed >= 0).then(|| hex(&digest))
}

fn sha256_file(path: &Path) -> io::Result<String> {
    let data = fs::read(path)?;
    sha256(&data).ok_or_else(|| io::Error::other("BCryptHash failed"))
}

/// Rejects a file whose hash is not pinned, unless this is a debug build.
fn check_hash(
    name: &str,
    actual: &str,
    pinned: &Option<String>,
    allow_unpinned: bool,
) -> Result<(), String> {
    match pinned {
        Some(expected) if expected == actual => Ok(()),
        Some(expected) => Err(format!(
            "{name} SHA-256 {actual} does not match the pinned {expected}"
        )),
        None if allow_unpinned => Ok(()),
        None => Err(format!(
            "{name} SHA-256 {actual} is not pinned in conpty.lock"
        )),
    }
}

/// Both files have to be present: the DLL starts `OpenConsole.exe` beside itself.
fn verify_files(dir: &Path, pins: &Pins, allow_unpinned: bool) -> Result<(), String> {
    let dll = dir.join("conpty.dll");
    let exe = dir.join("OpenConsole.exe");
    if !exe.is_file() {
        return Err(format!("{} is missing", exe.display()));
    }
    let dll_hash = sha256_file(&dll).map_err(|error| format!("{}: {error}", dll.display()))?;
    let exe_hash = sha256_file(&exe).map_err(|error| format!("{}: {error}", exe.display()))?;
    check_hash("conpty.dll", &dll_hash, &pins.conpty_dll, allow_unpinned)?;
    check_hash(
        "OpenConsole.exe",
        &exe_hash,
        &pins.open_console_exe,
        allow_unpinned,
    )
}

fn wide(text: &OsStr) -> Vec<u16> {
    text.encode_wide().chain(Some(0)).collect()
}

fn last_error(context: &str) -> io::Error {
    let code = unsafe { GetLastError() };
    io::Error::other(format!("{context}: win32 error {code}"))
}

fn candidate_directories() -> Vec<PathBuf> {
    let mut directories = Vec::new();
    if let Some(directory) = env::var_os(DIRECTORY_VARIABLE).map(PathBuf::from) {
        let directory = if directory.is_absolute() {
            directory
        } else {
            env::current_dir()
                .map(|cwd| cwd.join(&directory))
                .unwrap_or(directory)
        };
        directories.push(directory);
    }
    let exe = env::current_exe().ok();
    directories.extend(exe.as_deref().and_then(Path::parent).map(Path::to_path_buf));
    if cfg!(debug_assertions) {
        directories.push(Path::new(env!("CARGO_MANIFEST_DIR")).join("vendor/x64"));
    }
    directories
}

fn load_vendored(path: &Path) -> Option<Api> {
    unsafe {
        // The DLL directory serves the DLL's own dependencies; everything else comes from
        // System32. The current folder and PATH are deliberately not searched.
        let module = LoadLibraryExW(
            wide(path.as_os_str()).as_ptr(),
            null_mut(),
            LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32,
        );
        if module.is_null() {
            return None;
        }
        let create = GetProcAddress(module, c"ConptyCreatePseudoConsole".as_ptr().cast())?;
        let resize = GetProcAddress(module, c"ConptyResizePseudoConsole".as_ptr().cast())?;
        let close = GetProcAddress(module, c"ConptyClosePseudoConsole".as_ptr().cast())?;
        Some(Api {
            backend: Backend::Vendored,
            create: std::mem::transmute::<RawFn, CreateFn>(create),
            resize: std::mem::transmute::<RawFn, ResizeFn>(resize),
            close: std::mem::transmute::<RawFn, CloseFn>(close),
        })
    }
}

fn system_api() -> Api {
    Api {
        backend: Backend::System,
        create: CreatePseudoConsole,
        resize: ResizePseudoConsole,
        close: ClosePseudoConsole,
    }
}

fn select_api() -> Api {
    let pins = parse_pins(PINS);
    let allow_unpinned = cfg!(debug_assertions);
    let mut reason = None;
    for directory in candidate_directories() {
        let path = directory.join("conpty.dll");
        if !path.is_file() {
            continue;
        }
        if let Err(problem) = verify_files(&directory, &pins, allow_unpinned) {
            reason.get_or_insert(problem);
            continue;
        }
        if let Some(api) = load_vendored(&path) {
            return api;
        }
        reason.get_or_insert_with(|| format!("{} could not be loaded", path.display()));
    }
    if let Some(reason) = reason {
        eprintln!("terminal: {reason}; using the system ConPTY");
    }
    system_api()
}

/// The pinned check runs on the first terminal, off the UI thread, and the result is kept.
fn load_api() -> Api {
    static API: OnceLock<Api> = OnceLock::new();
    *API.get_or_init(select_api)
}

/// The caller gives up the handle: it has to be valid, with no other owner.
unsafe fn owned(handle: HANDLE) -> OwnedHandle {
    unsafe { OwnedHandle::from_raw_handle(handle) }
}

fn pipe() -> io::Result<(OwnedHandle, OwnedHandle)> {
    let (mut read, mut write) = (null_mut(), null_mut());
    if unsafe { CreatePipe(&mut read, &mut write, null(), 0) } == 0 {
        return Err(last_error("CreatePipe"));
    }
    Ok(unsafe { (owned(read), owned(write)) })
}

fn coord(cols: u16, rows: u16) -> COORD {
    COORD {
        X: cols as i16,
        Y: rows as i16,
    }
}

fn environment_block(env: &[(String, String)]) -> Vec<u16> {
    let mut sorted: Vec<_> = env.iter().collect();
    sorted.sort_by_key(|(key, _)| key.to_uppercase());
    let mut block = Vec::new();
    for (key, value) in sorted {
        block.extend(OsStr::new(&format!("{key}={value}")).encode_wide());
        block.push(0);
    }
    block.push(0);
    block
}

/// Quotes one argument the way `CommandLineToArgvW` reads it back.
fn push_argument(line: &mut String, argument: &str) {
    if !argument.is_empty() && !argument.contains([' ', '\t', '"']) {
        line.push_str(argument);
        return;
    }
    line.push('"');
    let mut backslashes = 0;
    for ch in argument.chars() {
        match ch {
            '\\' => backslashes += 1,
            '"' => {
                line.push_str(&"\\".repeat(backslashes * 2 + 1));
                line.push('"');
                backslashes = 0;
            }
            _ => {
                line.push_str(&"\\".repeat(backslashes));
                line.push(ch);
                backslashes = 0;
            }
        }
    }
    line.push_str(&"\\".repeat(backslashes * 2));
    line.push('"');
}

/// One line for `CreateProcessW` from a program and its arguments.
pub fn command_line(program: &Path, arguments: &[String]) -> String {
    let mut line = String::new();
    push_argument(&mut line, &program.to_string_lossy());
    for argument in arguments {
        line.push(' ');
        push_argument(&mut line, argument);
    }
    line
}

pub(crate) struct Pty {
    api: Api,
    console: Mutex<Option<HPCON>>,
    process: OwnedHandle,
    job: Job,
    pub(crate) pid: u32,
}

impl Pty {
    /// Returns the host plus the writer to the child's input and the reader of its output.
    pub(crate) fn spawn(spec: &SpawnSpec) -> io::Result<(Pty, File, File)> {
        // A child inherits ignoring Ctrl+C. Without a console of our own there is nothing to fix.
        let _ = allow_ctrl_c_in_children();
        let api = load_api();
        let flags = match api.backend {
            Backend::Vendored => GLYPH_WIDTH_WCSWIDTH,
            Backend::System => 0,
        };
        let (input_read, input_write) = pipe()?;
        let (output_read, output_write) = pipe()?;
        let size = coord(spec.cols, spec.rows);
        let mut console: HPCON = 0;
        let (input, output) = (input_read.as_raw_handle(), output_write.as_raw_handle());
        let result = unsafe { (api.create)(size, input, output, flags, &mut console) };
        if result < 0 {
            let code = result as u32;
            return Err(io::Error::other(format!(
                "CreatePseudoConsole: 0x{code:08x}"
            )));
        }
        // From here the console has to be closed on every path, and `Pty` does that.
        let job = match Job::new() {
            Ok(job) => job,
            Err(error) => {
                unsafe { (api.close)(console) };
                return Err(error);
            }
        };
        let (process, pid) = match start_in_job(spec, console, &job) {
            Ok(started) => started,
            Err(error) => {
                unsafe { (api.close)(console) };
                return Err(error);
            }
        };
        let pty = Pty {
            api,
            console: Mutex::new(Some(console)),
            process,
            job,
            pid,
        };
        Ok((pty, File::from(input_write), File::from(output_read)))
    }

    pub(crate) fn backend(&self) -> Backend {
        self.api.backend
    }

    pub(crate) fn resize(&self, cols: u16, rows: u16) -> io::Result<()> {
        let console = self.console.lock().unwrap();
        let Some(console) = *console else {
            return Ok(());
        };
        let result = unsafe { (self.api.resize)(console, coord(cols, rows)) };
        if result < 0 {
            let code = result as u32;
            return Err(io::Error::other(format!(
                "ResizePseudoConsole: 0x{code:08x}"
            )));
        }
        Ok(())
    }

    /// Blocks until the child exits.
    pub(crate) fn wait(&self) -> u32 {
        let process = self.process.as_raw_handle();
        unsafe {
            let mut code = 1;
            if WaitForSingleObject(process, INFINITE) == WAIT_OBJECT_0 {
                GetExitCodeProcess(process, &mut code);
            }
            code
        }
    }

    /// Ends the output stream. It may block until conhost has drained, so it has its own thread.
    pub(crate) fn close_console(&self) {
        let console = self.console.lock().unwrap().take();
        if let Some(console) = console {
            unsafe { (self.api.close)(console) };
        }
    }

    /// Kills the child and everything it started.
    pub(crate) fn terminate(&self) {
        let _ = self.job.terminate(1);
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        // The child tree goes first: a console whose program still runs keeps the close
        // waiting, and whoever drops the last session may be the window's thread.
        self.terminate();
        let console = self
            .console
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        let Some(console) = console else {
            return;
        };
        let close = self.api.close;
        // `ClosePseudoConsole` blocks until the console has drained, so the wait never
        // happens on the dropping thread. `HPCON` is a plain number, so a failed spawn
        // still has the handle to close here.
        let spawned = thread::Builder::new()
            .name("pty-close".to_string())
            .spawn(move || unsafe { close(console) });
        if spawned.is_err() {
            unsafe { (self.api.close)(console) };
        }
    }
}

/// Starts suspended so that no descendant can slip out before the job holds the child.
fn start_in_job(spec: &SpawnSpec, console: HPCON, job: &Job) -> io::Result<(OwnedHandle, u32)> {
    let (process, thread, pid) = unsafe { start_suspended(spec, console)? };
    if let Err(error) = job.assign(&process) {
        unsafe { TerminateProcess(process.as_raw_handle(), 1) };
        return Err(error);
    }
    unsafe { ResumeThread(thread.as_raw_handle()) };
    Ok((process, pid))
}

unsafe fn start_suspended(
    spec: &SpawnSpec,
    console: HPCON,
) -> io::Result<(OwnedHandle, OwnedHandle, u32)> {
    unsafe {
        let mut size = 0;
        InitializeProcThreadAttributeList(null_mut(), 1, 0, &mut size);
        let mut attributes = vec![0u8; size];
        let list = attributes.as_mut_ptr() as *mut c_void;
        if InitializeProcThreadAttributeList(list as _, 1, 0, &mut size) == 0 {
            return Err(last_error("InitializeProcThreadAttributeList"));
        }
        let attached = UpdateProcThreadAttribute(
            list as _,
            0,
            PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
            console as *const c_void,
            size_of::<HPCON>(),
            null_mut(),
            null(),
        );
        if attached == 0 {
            DeleteProcThreadAttributeList(list as _);
            return Err(last_error("UpdateProcThreadAttribute"));
        }

        let mut startup: STARTUPINFOEXW = zeroed();
        startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
        // Null std handles stop a redirected parent from leaking its pipes past the pseudoconsole.
        startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        startup.StartupInfo.hStdInput = INVALID_HANDLE_VALUE;
        startup.StartupInfo.hStdOutput = INVALID_HANDLE_VALUE;
        startup.StartupInfo.hStdError = INVALID_HANDLE_VALUE;
        startup.lpAttributeList = list as _;

        let mut command = wide(OsStr::new(&spec.command_line));
        let cwd = spec.cwd.as_ref().map(|path| wide(path.as_os_str()));
        let env = environment_block(&spec.env);
        let mut info: PROCESS_INFORMATION = zeroed();
        let created = CreateProcessW(
            null(),
            command.as_mut_ptr(),
            null(),
            null(),
            0,
            EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT | CREATE_SUSPENDED,
            env.as_ptr() as *const c_void,
            cwd.as_ref().map_or(null(), |path| path.as_ptr()),
            &startup.StartupInfo,
            &mut info,
        );
        let result = if created == 0 {
            Err(last_error(&format!(
                "CreateProcessW({})",
                spec.command_line
            )))
        } else {
            Ok((owned(info.hProcess), owned(info.hThread), info.dwProcessId))
        };
        DeleteProcThreadAttributeList(list as _);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(arguments: &[&str]) -> String {
        let arguments: Vec<String> = arguments.iter().map(|text| text.to_string()).collect();
        command_line(Path::new("tool.exe"), &arguments)
    }

    #[test]
    fn arguments_are_quoted_for_argv() {
        assert_eq!(line(&["plain"]), "tool.exe plain");
        assert_eq!(line(&[""]), r#"tool.exe """#);
        assert_eq!(line(&["two words"]), r#"tool.exe "two words""#);
        assert_eq!(line(&[r#"say "hi""#]), r#"tool.exe "say \"hi\"""#);
        assert_eq!(line(&[r"C:\a b\"]), r#"tool.exe "C:\a b\\""#);
        assert_eq!(line(&[r"C:\plain\"]), r"tool.exe C:\plain\");
    }

    struct TempDir(PathBuf, #[allow(dead_code)] test_support::TempDir);

    impl TempDir {
        fn new(name: &str) -> Self {
            let folder = test_support::TempDir::new(&format!("term-conpty-{name}"));
            Self(folder.path().to_path_buf(), folder)
        }

        fn with_files(&self, dll: &[u8], exe: &[u8]) -> Pins {
            std::fs::write(self.0.join("conpty.dll"), dll).unwrap();
            std::fs::write(self.0.join("OpenConsole.exe"), exe).unwrap();
            Pins {
                conpty_dll: Some(sha256(dll).unwrap()),
                open_console_exe: Some(sha256(exe).unwrap()),
            }
        }
    }

    #[test]
    fn the_lock_parses_empty_and_filled_values() {
        let empty = parse_pins("conpty_dll_sha256 =\nopen_console_exe_sha256 =\n");
        assert_eq!(empty, Pins::default());

        let hash = "a".repeat(SHA256_HEX);
        let filled = parse_pins(&format!(
            "# comment\nconpty_dll_sha256 = {hash}\nopen_console_exe_sha256 = \"{hash}\"\n"
        ));
        assert_eq!(filled.conpty_dll.as_deref(), Some(hash.as_str()));
        assert_eq!(filled.open_console_exe.as_deref(), Some(hash.as_str()));

        let malformed = parse_pins("conpty_dll_sha256 = nope\n");
        assert_eq!(malformed, Pins::default());
    }

    #[test]
    fn matching_hashes_pass_and_mismatches_fail() {
        let temp = TempDir::new("hashes");
        let pins = temp.with_files(b"dll", b"exe");
        assert_eq!(verify_files(&temp.0, &pins, false), Ok(()));

        let wrong = Pins {
            conpty_dll: Some("0".repeat(SHA256_HEX)),
            ..pins.clone()
        };
        let problem = verify_files(&temp.0, &wrong, false).unwrap_err();
        assert!(problem.contains("conpty.dll"), "{problem}");
        assert!(problem.contains("does not match"), "{problem}");

        let wrong = Pins {
            open_console_exe: Some("0".repeat(SHA256_HEX)),
            ..pins
        };
        let problem = verify_files(&temp.0, &wrong, false).unwrap_err();
        assert!(problem.contains("OpenConsole.exe"), "{problem}");
    }

    #[test]
    fn an_empty_lock_is_rejected_in_release_and_allowed_in_debug() {
        let temp = TempDir::new("empty-lock");
        temp.with_files(b"dll", b"exe");
        let problem = verify_files(&temp.0, &Pins::default(), false).unwrap_err();
        assert!(problem.contains("not pinned"), "{problem}");
        assert_eq!(verify_files(&temp.0, &Pins::default(), true), Ok(()));
    }

    #[test]
    fn the_debug_allowance_does_not_skip_a_pinned_mismatch() {
        let temp = TempDir::new("debug-mismatch");
        let pins = temp.with_files(b"dll", b"exe");
        let wrong = Pins {
            conpty_dll: Some("1".repeat(SHA256_HEX)),
            ..pins
        };
        assert!(verify_files(&temp.0, &wrong, true).is_err());
    }

    #[test]
    fn openconsole_has_to_sit_next_to_the_dll() {
        let temp = TempDir::new("no-openconsole");
        std::fs::write(temp.0.join("conpty.dll"), b"dll").unwrap();
        let problem = verify_files(&temp.0, &Pins::default(), true).unwrap_err();
        assert!(problem.contains("OpenConsole.exe"), "{problem}");
        assert!(problem.contains("missing"), "{problem}");
    }

    #[test]
    fn dropping_a_live_console_does_not_wait_for_its_output() {
        let spec = SpawnSpec {
            command_line: "ping.exe -t 127.0.0.1".to_string(),
            cwd: None,
            env: crate::env::allowlisted(),
            cols: 80,
            rows: 24,
            shift_enter: Default::default(),
        };
        let (pty, input, output) = Pty::spawn(&spec).expect("spawn");
        // Let the program fill the output pipe before the console goes away.
        std::thread::sleep(std::time::Duration::from_millis(500));
        // Nobody drains the pipe here, so a close that waits for the console comes back
        // only when the test gives the pipe up: the dropping thread must not be the
        // window's, and the window may not wait for it either.
        let (done, finished) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            drop(pty);
            let _ = done.send(());
        });
        let waited = finished.recv_timeout(std::time::Duration::from_secs(10));
        drop(input);
        drop(output);
        assert!(
            waited.is_ok(),
            "closing the console waited for its output pipe"
        );
    }
}

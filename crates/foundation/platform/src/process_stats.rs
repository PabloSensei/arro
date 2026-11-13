//! Memory of this process and of the processes it started, by role.
//!
//! A role is a short label the launcher gives a child, such as `Terminal` or `Build tool`.
//! It is kept beside the process id only, so a child started outside that path still
//! shows its image name. The tree is read from a toolhelp snapshot: no WMI and no call
//! that waits for a process.

use std::collections::HashMap;
use std::ffi::OsString;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::mem::size_of;
use std::os::windows::ffi::OsStringExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_INVALID_PARAMETER, FILETIME, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::ProcessStatus::{
    GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX2,
};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcessId, GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    PROCESS_TERMINATE, PROCESS_VM_READ, QueryFullProcessImageNameW, TerminateProcess,
};

/// The `Subsystem` field of a PE optional header: a window program, or a console program.
const IMAGE_SUBSYSTEM_WINDOWS_GUI: u16 = 2;
const IMAGE_SUBSYSTEM_WINDOWS_CUI: u16 = 3;
/// The optional header starts after the PE signature and the 20-byte COFF header.
const PE_OPTIONAL_HEADER: u64 = 24;
/// `Subsystem` lies at the same offset in both the 32-bit and the 64-bit optional header.
const PE_SUBSYSTEM: usize = 68;

/// The memory of one process of the tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessMemory {
    pub pid: u32,
    pub parent: Option<u32>,
    /// The image name, such as `arro.exe`.
    pub name: String,
    /// The label its launcher gave it, when there is one.
    pub role: Option<&'static str>,
    pub working_set: u64,
    pub private_bytes: u64,
    pub private_working_set: u64,
}

/// Gives a child a role for the memory table. Harmless once the process is gone.
pub fn label_process(pid: u32, role: &'static str) {
    roles()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .insert(pid, role);
}

/// One live process of the machine, as a toolhelp snapshot reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessEntry {
    pub pid: u32,
    pub parent: Option<u32>,
    pub name: String,
}

/// Every live process with its parent. The id of a dead parent is kept, so a program
/// that outlived its shell still names it, and a walk can find it after the shell is gone.
pub fn process_table() -> Vec<ProcessEntry> {
    snapshot()
        .into_iter()
        .map(|entry| ProcessEntry {
            pid: entry.pid,
            parent: (entry.parent != 0).then_some(entry.parent),
            name: entry.name,
        })
        .collect()
}

/// When the process was created, in the units of `FILETIME`. Two processes never share
/// it, so a caller can tell its own process from one that took the same id later.
pub fn process_created(pid: u32) -> Option<u64> {
    // SAFETY: the handle is checked and closed below.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        return None;
    }
    // SAFETY: a non-null result is ours to own.
    let handle = unsafe { OwnedHandle::from_raw_handle(handle) };
    creation_time(handle.as_raw_handle())
}

/// Whether the image behind `pid` is a console program, from the `Subsystem` field of its
/// PE header. `None` when the image cannot be read, and the caller then leaves the process
/// alone: ending a program that might not be a console one is worse than one left running.
///
/// The subsystem belongs to the file, so the answer is cached by image path.
pub fn is_console_process(pid: u32) -> Option<bool> {
    let path = image_path(pid)?;
    let mut cache = subsystems()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    if let Some(known) = cache.get(&path) {
        return *known;
    }
    let console = subsystem_of(&path);
    cache.insert(path, console);
    console
}

fn subsystems() -> &'static Mutex<HashMap<PathBuf, Option<bool>>> {
    static SUBSYSTEMS: OnceLock<Mutex<HashMap<PathBuf, Option<bool>>>> = OnceLock::new();
    SUBSYSTEMS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The image path of a running process, as the loader opened it.
fn image_path(pid: u32) -> Option<PathBuf> {
    // SAFETY: the handle is checked and closed below.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        return None;
    }
    // SAFETY: a non-null result is ours to own.
    let handle = unsafe { OwnedHandle::from_raw_handle(handle) };
    let mut buffer = [0u16; 1024];
    let mut length = buffer.len() as u32;
    // SAFETY: the buffer is writable for `length` characters, and the handle is valid.
    let done = unsafe {
        QueryFullProcessImageNameW(handle.as_raw_handle(), 0, buffer.as_mut_ptr(), &mut length)
    };
    (done != 0).then(|| PathBuf::from(OsString::from_wide(&buffer[..length as usize])))
}

/// Reads the PE header of an image: `true` for a console program, `false` for a window
/// program, `None` for any other subsystem or a header that cannot be read.
fn subsystem_of(path: &Path) -> Option<bool> {
    let mut file = File::open(path).ok()?;
    let mut dos = [0u8; 64];
    file.read_exact(&mut dos).ok()?;
    // `e_lfanew` at 0x3c points at the PE signature.
    let offset = u32::from_le_bytes([dos[0x3c], dos[0x3d], dos[0x3e], dos[0x3f]]) as u64;
    file.seek(SeekFrom::Start(offset + PE_OPTIONAL_HEADER))
        .ok()?;
    // The magic tells PE32 from PE32+; `Subsystem` follows at the same offset in both.
    let mut optional = [0u8; PE_SUBSYSTEM + 2];
    file.read_exact(&mut optional).ok()?;
    let magic = u16::from_le_bytes([optional[0], optional[1]]);
    if magic != 0x10b && magic != 0x20b {
        return None;
    }
    match u16::from_le_bytes([optional[PE_SUBSYSTEM], optional[PE_SUBSYSTEM + 1]]) {
        IMAGE_SUBSYSTEM_WINDOWS_GUI => Some(false),
        IMAGE_SUBSYSTEM_WINDOWS_CUI => Some(true),
        _ => None,
    }
}

/// Ends one process, but only while the id still names the one created at `created`.
/// Returns `Ok` when the process is already gone or the id was reused.
pub fn terminate_process(pid: u32, created: u64) -> io::Result<()> {
    // SAFETY: the handle is checked and closed below.
    let handle = unsafe {
        OpenProcess(
            PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION,
            0,
            pid,
        )
    };
    if handle.is_null() {
        let error = io::Error::last_os_error();
        return match error.raw_os_error() {
            Some(code) if code == ERROR_INVALID_PARAMETER as i32 => Ok(()),
            _ => Err(error),
        };
    }
    // SAFETY: a non-null result is ours to own.
    let handle = unsafe { OwnedHandle::from_raw_handle(handle) };
    if creation_time(handle.as_raw_handle()) != Some(created) {
        // The id belongs to another process now, which is not ours to end.
        return Ok(());
    }
    // SAFETY: the handle names a process that is still the one we opened.
    if unsafe { TerminateProcess(handle.as_raw_handle(), 1) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn creation_time(handle: HANDLE) -> Option<u64> {
    // SAFETY: an all-zero `FILETIME` is valid; the call writes all four.
    let (mut created, mut exited, mut kernel, mut user): (FILETIME, FILETIME, FILETIME, FILETIME) =
        unsafe { std::mem::zeroed() };
    // SAFETY: the handle is valid and every pointer is writable for the call.
    let done =
        unsafe { GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user) };
    (done != 0).then_some(((created.dwHighDateTime as u64) << 32) | created.dwLowDateTime as u64)
}

/// This process and every process below it, in the order they were found.
pub fn process_tree() -> Vec<ProcessMemory> {
    let root = unsafe { GetCurrentProcessId() };
    let processes = snapshot();
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    for process in &processes {
        children
            .entry(process.parent)
            .or_default()
            .push(process.pid);
    }
    let mut found = Vec::new();
    let mut pending = vec![root];
    while let Some(pid) = pending.pop() {
        if let Some(process) = processes.iter().find(|process| process.pid == pid) {
            found.push(ProcessMemory {
                pid: process.pid,
                parent: Some(process.parent),
                name: process.name.clone(),
                role: None,
                working_set: 0,
                private_bytes: 0,
                private_working_set: 0,
            });
        }
        if let Some(below) = children.get(&pid) {
            pending.extend(below.iter().copied());
        }
    }
    prune_roles(&processes);
    let current = root;
    for process in &mut found {
        process.role = if process.pid == current {
            Some("Arro")
        } else {
            role_of(process.pid)
        };
        if let Some(memory) = process_memory(process.pid) {
            process.working_set = memory.0;
            process.private_bytes = memory.1;
            process.private_working_set = memory.2;
        }
    }
    found.sort_by_key(|process| (process.pid != current, process.pid));
    found
}

#[derive(Clone)]
struct Entry {
    pid: u32,
    parent: u32,
    name: String,
}

fn snapshot() -> Vec<Entry> {
    // SAFETY: the call returns a handle that this function closes, or an invalid one.
    let handle: HANDLE = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if handle == INVALID_HANDLE_VALUE {
        return Vec::new();
    }
    let mut entries = Vec::new();
    // SAFETY: an all-zero entry is valid, and `dwSize` tells the call its layout.
    let mut entry: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
    entry.dwSize = size_of::<PROCESSENTRY32W>() as u32;
    // SAFETY: the handle is valid and the entry is writable for the call.
    let mut more = unsafe { Process32FirstW(handle, &mut entry) } != 0;
    while more {
        let name = name_of(&entry);
        entries.push(Entry {
            pid: entry.th32ProcessID,
            parent: entry.th32ParentProcessID,
            name,
        });
        // SAFETY: the same handle and entry as above.
        more = unsafe { Process32NextW(handle, &mut entry) } != 0;
    }
    // SAFETY: the handle came from the snapshot and is not used again.
    unsafe {
        CloseHandle(handle);
    }
    entries
}

fn name_of(entry: &PROCESSENTRY32W) -> String {
    let length = entry
        .szExeFile
        .iter()
        .position(|character| *character == 0)
        .unwrap_or(entry.szExeFile.len());
    OsString::from_wide(&entry.szExeFile[..length])
        .to_string_lossy()
        .into_owned()
}

fn process_memory(pid: u32) -> Option<(u64, u64, u64)> {
    // SAFETY: the handle is checked and closed below.
    let handle =
        unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ, 0, pid) };
    if handle.is_null() {
        return None;
    }
    // SAFETY: the counters start zeroed and `cb` carries their size.
    let mut counters: PROCESS_MEMORY_COUNTERS_EX2 = unsafe { std::mem::zeroed() };
    counters.cb = size_of::<PROCESS_MEMORY_COUNTERS_EX2>() as u32;
    // SAFETY: the size in `cb` describes the buffer this pointer shows.
    let done = unsafe { GetProcessMemoryInfo(handle, (&raw mut counters).cast(), counters.cb) };
    // SAFETY: the handle came from `OpenProcess` and is not used again.
    unsafe {
        CloseHandle(handle);
    }
    if done == 0 {
        return None;
    }
    Some((
        counters.WorkingSetSize as u64,
        counters.PrivateUsage as u64,
        counters.PrivateWorkingSetSize as u64,
    ))
}

fn roles() -> &'static Mutex<HashMap<u32, &'static str>> {
    static ROLES: OnceLock<Mutex<HashMap<u32, &'static str>>> = OnceLock::new();
    ROLES.get_or_init(|| Mutex::new(HashMap::new()))
}

fn role_of(pid: u32) -> Option<&'static str> {
    roles()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .get(&pid)
        .copied()
}

fn prune_roles(alive: &[Entry]) {
    let mut roles = roles().lock().unwrap_or_else(|poison| poison.into_inner());
    roles.retain(|pid, _| alive.iter().any(|process| process.pid == *pid));
}

#[cfg(test)]
mod tests {
    use super::{is_console_process, label_process, process_tree, subsystem_of};
    use std::path::PathBuf;
    use windows_sys::Win32::System::Threading::GetCurrentProcessId;

    #[test]
    fn the_subsystem_of_an_image_comes_from_its_header() {
        let root = std::env::var_os("SystemRoot").expect("SystemRoot");
        let root = PathBuf::from(root).join("System32");
        assert_eq!(subsystem_of(&root.join("ping.exe")), Some(true));
        assert_eq!(subsystem_of(&root.join("notepad.exe")), Some(false));
        // An image that is not there is unknown, and the caller leaves it alone.
        assert_eq!(subsystem_of(&root.join("no-such-image.exe")), None);
        // The test binary itself is a console program.
        assert_eq!(is_console_process(std::process::id()), Some(true));
    }

    #[test]
    fn the_tree_starts_with_this_process() {
        let tree = process_tree();
        let current = unsafe { GetCurrentProcessId() };
        assert_eq!(tree.first().map(|process| process.pid), Some(current));
        assert_eq!(tree[0].role, Some("Arro"));
        assert!(
            tree[0].working_set > 0,
            "the memory of this process is read"
        );
        assert!(tree[0].private_bytes > 0);
        assert!(tree[0].private_working_set > 0);
    }

    #[test]
    fn a_role_is_kept_for_a_live_process() {
        let current = unsafe { GetCurrentProcessId() };
        label_process(current, "Test process");
        let tree = process_tree();
        // The current process keeps the role of Arro, not the label of a child.
        assert_eq!(tree[0].role, Some("Arro"));
    }
}

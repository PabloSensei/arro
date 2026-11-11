//! Memory of this process and of the processes it started, by role.
//!
//! A role is a short label the launcher gives a child, such as `Terminal` or `Build tool`.
//! It is kept beside the process id only, so a child started outside that path still
//! shows its image name. The tree is read from a toolhelp snapshot: no WMI and no call
//! that waits for a process.

use std::collections::HashMap;
use std::ffi::OsString;
use std::mem::size_of;
use std::os::windows::ffi::OsStringExt;
use std::sync::{Mutex, OnceLock};

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::ProcessStatus::{
    GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX2,
};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcessId, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_VM_READ,
};

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
    use super::{label_process, process_tree};
    use windows_sys::Win32::System::Threading::GetCurrentProcessId;

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

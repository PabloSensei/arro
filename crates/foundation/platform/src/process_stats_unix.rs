//! Memory of this process and of the processes it started, by role.
//!
//! A role is a short label the launcher gives a child, such as `Terminal` or `Build tool`.
//! It is kept beside the process id only, so a child started outside that path still
//! shows its image name. The tree is read from `/proc`: no call waits for a process.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// The memory of one process of the tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessMemory {
    pub pid: u32,
    pub parent: Option<u32>,
    /// The image name, such as `arro`.
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
    let root = std::process::id();
    let processes = snapshot();
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    for process in &processes {
        if let Some(parent) = process.parent {
            children.entry(parent).or_default().push(process.pid);
        }
    }
    let mut found = Vec::new();
    let mut pending = vec![root];
    while let Some(pid) = pending.pop() {
        if let Some(process) = processes.iter().find(|process| process.pid == pid) {
            found.push(ProcessMemory {
                pid: process.pid,
                parent: process.parent,
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
    for process in &mut found {
        process.role = if process.pid == root {
            Some("Arro")
        } else {
            role_of(process.pid)
        };
        if let Some(memory) = process_memory(process.pid) {
            process.working_set = memory.0;
            process.private_bytes = memory.1;
            process.private_working_set = memory.1;
        }
    }
    found.sort_by_key(|process| (process.pid != root, process.pid));
    found
}

#[derive(Clone)]
struct Entry {
    pid: u32,
    parent: Option<u32>,
    name: String,
}

fn snapshot() -> Vec<Entry> {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    let mut processes = Vec::new();
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        let Some(stat) = stat_of(pid) else {
            continue;
        };
        if !stat.alive {
            continue;
        }
        processes.push(Entry {
            pid,
            parent: stat.parent,
            name: name_of(pid),
        });
    }
    processes
}

struct Stat {
    parent: Option<u32>,
    group: i32,
    /// A zombie or a dead process is not alive: it keeps no code and answers no signal.
    alive: bool,
}

/// The parent and the process group from `/proc/<pid>/stat`. The command name in the
/// second field may hold spaces and parentheses, so parsing starts after the last `)`.
fn stat_of(pid: u32) -> Option<Stat> {
    let text = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest = &text[text.rfind(')')? + 1..];
    let mut fields = rest.split_whitespace();
    let state = fields.next()?.as_bytes().first().copied()?;
    let parent = fields.next()?.parse().ok();
    let group = fields.next()?.parse().ok()?;
    Some(Stat {
        parent,
        group,
        alive: state != b'Z' && state != b'X',
    })
}

/// The process group of a living process, or `None` when it is gone. A zombie is gone
/// for a caller that wants to end the group: nothing is left to signal.
pub(crate) fn process_group(pid: u32) -> Option<i32> {
    stat_of(pid)
        .filter(|stat| stat.alive)
        .map(|stat| stat.group)
}

fn name_of(pid: u32) -> String {
    std::fs::read_to_string(format!("/proc/{pid}/comm"))
        .map(|name| name.trim().to_owned())
        .unwrap_or_default()
}

/// Resident set, then private bytes. Linux has no private working set of its own, so the
/// shared pages are taken out of the resident set.
fn process_memory(pid: u32) -> Option<(u64, u64, u64)> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    let resident = status.lines().find_map(|line| {
        let value = line.strip_prefix("VmRSS:")?;
        let kilobytes = value.split_whitespace().next()?.parse::<u64>().ok()?;
        Some(kilobytes * 1024)
    })?;
    let private = private_bytes(pid).unwrap_or(resident);
    Some((resident, private, private))
}

fn private_bytes(pid: u32) -> Option<u64> {
    let text = std::fs::read_to_string(format!("/proc/{pid}/statm")).ok()?;
    let mut fields = text.split_whitespace();
    let resident: u64 = fields.next()?.parse().ok()?;
    let shared: u64 = fields.next()?.parse().ok()?;
    // SAFETY: a plain query of an unchanged system value.
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as u64;
    Some(resident.saturating_sub(shared).saturating_mul(page))
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

    #[test]
    fn the_tree_starts_with_this_process() {
        let tree = process_tree();
        let current = std::process::id();
        assert_eq!(tree.first().map(|process| process.pid), Some(current));
        assert_eq!(tree[0].role, Some("Arro"));
        assert!(
            tree[0].working_set > 0,
            "the memory of this process is read"
        );
    }

    #[test]
    fn a_role_is_kept_for_a_live_process() {
        let current = std::process::id();
        label_process(current, "Test process");
        let tree = process_tree();
        // The current process keeps the role of Arro, not the label of a child.
        assert_eq!(tree[0].role, Some("Arro"));
    }
}

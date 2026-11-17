//! Console program for the tests of this crate: shows what really reaches a program.
//! `lines` hashes stdin up to a line `END`, `keys` prints console key records, `sleep` writes
//! READY into a file and waits, `children <pid>` lists the children of a process, `attach <line>`
//! hosts a command in this console.

#[cfg(windows)]
use std::io::Write;
use std::io::{self, BufRead};
#[cfg(windows)]
use std::mem::{size_of, zeroed};
#[cfg(windows)]
use std::sync::Arc;
use std::time::Duration;

#[cfg(windows)]
use terminal::{ShiftEnter, SpawnSpec, TerminalSession, allowlisted, attach_console, console_size};
#[cfg(windows)]
use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
#[cfg(windows)]
use windows_sys::Win32::System::Console::{
    GetStdHandle, INPUT_RECORD, ReadConsoleInputW, STD_INPUT_HANDLE,
};
#[cfg(windows)]
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};

#[cfg(windows)]
const KEY_EVENT: u16 = 1;

fn sleep(file: &str) {
    std::fs::write(file, "READY").expect("write");
    std::thread::sleep(Duration::from_secs(600));
}

#[cfg(windows)]
fn children(parent: u32) {
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        assert!(snapshot != INVALID_HANDLE_VALUE);
        let mut entry: PROCESSENTRY32W = zeroed();
        entry.dwSize = size_of::<PROCESSENTRY32W>() as u32;
        let mut more = Process32FirstW(snapshot, &mut entry) != 0;
        while more {
            if entry.th32ParentProcessID == parent && entry.th32ProcessID != std::process::id() {
                let end = entry.szExeFile.iter().position(|&unit| unit == 0);
                let name = &entry.szExeFile[..end.unwrap_or(0)];
                println!("{}", String::from_utf16_lossy(name));
            }
            more = Process32NextW(snapshot, &mut entry) != 0;
        }
        CloseHandle(snapshot);
    }
}

fn lines() {
    let (mut bytes, mut hash) = (0u64, 0xcbf29ce484222325u64);
    for line in io::stdin().lock().lines() {
        let line = line.expect("stdin");
        if line == "END" {
            break;
        }
        bytes += line.len() as u64;
        for byte in line.bytes() {
            hash = (hash ^ byte as u64).wrapping_mul(0x100000001b3);
        }
    }
    println!("TOTAL {bytes} {hash:016x}");
}

#[cfg(windows)]
fn keys() {
    println!("READY");
    io::stdout().flush().expect("stdout");
    let input = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
    loop {
        let mut record: INPUT_RECORD = unsafe { zeroed() };
        let mut count = 0;
        if unsafe { ReadConsoleInputW(input, &mut record, 1, &mut count) } == 0 {
            return;
        }
        if record.EventType != KEY_EVENT {
            continue;
        }
        let key = unsafe { record.Event.KeyEvent };
        if key.bKeyDown != 0 {
            let unicode = unsafe { key.uChar.UnicodeChar };
            println!(
                "KEY {} {} {:#x}",
                key.wVirtualKeyCode, unicode, key.dwControlKeyState
            );
            io::stdout().flush().expect("stdout");
            // F10 ends the run.
            if key.wVirtualKeyCode == 0x79 {
                return;
            }
        }
    }
}

#[cfg(windows)]
fn attach(command_line: &str) {
    let (cols, rows) = console_size().expect("a console");
    let session = TerminalSession::spawn(&SpawnSpec {
        command_line: command_line.to_string(),
        cwd: None,
        env: allowlisted(),
        cols,
        rows,
        shift_enter: ShiftEnter::default(),
    })
    .expect("spawn");
    attach_console(Arc::new(session)).expect("attach");
}

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let arguments: Vec<&str> = arguments.iter().map(String::as_str).collect();
    match arguments.as_slice() {
        ["lines"] => lines(),
        ["sleep", file] => sleep(file),
        #[cfg(windows)]
        ["keys"] => keys(),
        #[cfg(windows)]
        ["children", pid] => children(pid.parse().expect("pid")),
        #[cfg(windows)]
        ["attach", line] => attach(line),
        _ => eprintln!("usage: term-probe lines|keys|sleep <file>|children <pid>|attach <line>"),
    }
}

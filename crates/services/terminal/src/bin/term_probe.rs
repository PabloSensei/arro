//! Console program for the tests of this crate: shows what really reaches a program.
//! `lines` hashes stdin up to a line `END`, `keys` prints console key records, `sleep` writes
//! READY into a file and waits, `children <pid>` lists the children of a process, `attach <line>`
//! hosts a command in this console, `tui <modes>` sets the modes it names and prints the raw bytes
//! of its input.

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
    ENABLE_VIRTUAL_TERMINAL_INPUT, ENABLE_VIRTUAL_TERMINAL_PROCESSING, GetConsoleMode,
    GetStdHandle, INPUT_RECORD, ReadConsoleInputW, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
    SetConsoleMode,
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

/// A full-screen program of the wheel tests. It sets the modes its arguments name, prints READY and
/// then prints what each read of its input brings, with the escapes made visible.
#[cfg(windows)]
fn tui(modes: &[&str]) {
    use std::io::Read;

    let mut picture = String::new();
    for mode in modes {
        match *mode {
            "alt" => picture.push_str("\x1b[?1049h\x1b[2J\x1b[H"),
            "mouse" => picture.push_str("\x1b[?1000h"),
            "drag" => picture.push_str("\x1b[?1002h"),
            "motion" => picture.push_str("\x1b[?1003h"),
            "sgr" => picture.push_str("\x1b[?1006h"),
            "cursor" => picture.push_str("\x1b[?1h"),
            "erase" => picture.push_str("\x1b[2J\x1b[3J\x1b[H"),
            "lines" => {
                for number in 1..=60 {
                    picture.push_str(&format!("line {number:02}\r\n"));
                }
            }
            _ => {}
        }
    }
    // The escapes above are commands only with VT output on; the input is raw bytes, without
    // line editing and without echo.
    unsafe {
        let output = GetStdHandle(STD_OUTPUT_HANDLE);
        let mut mode = 0;
        if GetConsoleMode(output, &mut mode) != 0 {
            SetConsoleMode(output, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING);
        }
        SetConsoleMode(
            GetStdHandle(STD_INPUT_HANDLE),
            ENABLE_VIRTUAL_TERMINAL_INPUT,
        );
    }
    // No line break after READY: a line break there would scroll the screen after the test sees
    // READY, and a wheel turn sent in between would then land one line off.
    print!("{picture}READY");
    io::stdout().flush().expect("stdout");
    let mut buffer = [0u8; 256];
    loop {
        let count = match io::stdin().read(&mut buffer) {
            Ok(0) | Err(_) => return,
            Ok(count) => count,
        };
        let shown: String = buffer[..count]
            .iter()
            .map(|&byte| match byte {
                0x1b => "^[".to_string(),
                b'\r' => "^M".to_string(),
                other => char::from(other).to_string(),
            })
            .collect();
        print!("\r\nGOT {shown}");
        io::stdout().flush().expect("stdout");
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
        #[cfg(windows)]
        ["tui", modes @ ..] => tui(modes),
        _ => eprintln!(
            "usage: term-probe lines|keys|sleep <file>|children <pid>|attach <line>|tui <modes>"
        ),
    }
}

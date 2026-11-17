//! Sends Ctrl+C or Ctrl+Break to the console of another process. It has to be a process of
//! its own: attaching to a console takes the caller off its own.

use std::process::ExitCode;

use platform::{ConsoleSignal, signal_console_of};

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let signal = match args.next().as_deref() {
        Some("--ctrl-c") => ConsoleSignal::CtrlC,
        Some("--ctrl-break") => ConsoleSignal::CtrlBreak,
        _ => return usage(),
    };
    let Some(process_id) = args.next().and_then(|arg| arg.parse().ok()) else {
        return usage();
    };
    if let Err(error) = signal_console_of(process_id, signal) {
        eprintln!("ide-ctrl: process {process_id}: {error}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

fn usage() -> ExitCode {
    eprintln!("usage: ide-ctrl --ctrl-c|--ctrl-break <pid>");
    ExitCode::from(2)
}

//! A window program of the tests of this crate: no window and no output, it only lives
//! until someone ends it. The GUI subsystem is the point: a program like this one, started
//! beside a console tree, has to outlive the process that started it.

#![cfg_attr(windows, windows_subsystem = "windows")]

use std::time::Duration;

fn main() {
    std::thread::sleep(Duration::from_secs(600));
}

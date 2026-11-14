//! A stand-in for the database helper in tests.
//!
//! It says that it is ready and then keeps its pipes open until its owner ends it. A test
//! of the session uses it to see that a helper does not outlive the session that started
//! it, on a machine without a driver.

use std::io::{self, BufRead, Write};

const READY: &str = r#"{"type":"ready","protocol":1,"vendor":"stand-in","version":"1"}"#;

fn main() -> io::Result<()> {
    let stdout = io::stdout();
    let mut out = stdout.lock();
    out.write_all(READY.as_bytes())?;
    out.write_all(b"\n")?;
    out.flush()?;
    for line in io::stdin().lock().lines() {
        let line = line?;
        if line.contains(r#""type":"close""#) {
            break;
        }
    }
    Ok(())
}

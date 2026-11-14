//! A stand-in for a console helper in tests.
//!
//! It says that it is ready, answers a completion and keeps its pipes open until its
//! owner ends it. A test of the session runs it on every machine, with no runtime.

use std::io::{self, BufRead, Write};

const READY: &str = r#"{"type":"ready","protocol":1,"runtime":"stand-in","engine":"local"}"#;

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
        if line.contains(r#""type":"complete""#) {
            let id = line
                .split(r#""id":"#)
                .nth(1)
                .and_then(|tail| tail.split(',').next())
                .unwrap_or("0");
            let answer = format!(
                r#"{{"type":"completion","id":{id},"items":[{{"text":"substring(","matchesType":true}}]}}"#
            );
            out.write_all(answer.as_bytes())?;
            out.write_all(b"\n")?;
            out.flush()?;
        }
    }
    Ok(())
}

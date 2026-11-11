//! Writes a panic to the log folder before the process ends: the release build has no
//! console, so a crash would otherwise leave no message at all.
//!
//! A frame is `module+0xoffset`. A function name is added only when a `.pdb` sits next
//! to the module, which is the debug build; without it the nearest export has nothing
//! to do with the frame.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use backtrace::{Backtrace, BacktraceFrame};

/// Writes every panic to `dir`, then runs the previous hook.
pub fn install(dir: PathBuf) {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let message = info
            .payload_as_str()
            .unwrap_or("<the panic payload is not text>");
        let location = info
            .location()
            .map(|at| (at.file(), at.line(), at.column()));
        let thread = std::thread::current();
        let thread = thread.name().unwrap_or("unnamed");
        let backtrace = format_frames(Backtrace::new_unresolved());
        let report = format_log(message, thread, location, &backtrace);
        let _ = write_report(&dir, SystemTime::now(), &report);
        previous(info);
    }));
}

/// The text of one report. Kept apart from the hook so that a test can read it.
fn format_report(
    message: &str,
    thread: &str,
    location: Option<(&str, u32, u32)>,
    backtrace: &str,
) -> String {
    let place = location.map_or_else(
        || "unknown".to_string(),
        |(file, line, column)| format!("{file}:{line}:{column}"),
    );
    format!("panic: {message}\nthread: {thread}\nlocation: {place}\nbacktrace:\n{backtrace}\n")
}

/// A report with the three lines that name the build, so the log of a user can be read
/// with the symbols of that release.
fn format_log(
    message: &str,
    thread: &str,
    location: Option<(&str, u32, u32)>,
    backtrace: &str,
) -> String {
    format!(
        "version: {}\ncommit: {}\nos: {}\n{}",
        env!("IDE_VERSION"),
        env!("IDE_COMMIT"),
        platform::os_version(),
        format_report(message, thread, location, backtrace)
    )
}

fn write_report(dir: &Path, time: SystemTime, report: &str) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join(file_name(time));
    let mut file = std::fs::File::create(&path)?;
    file.write_all(report.as_bytes())?;
    Ok(path)
}

/// A name without colons, so that it works on Windows too.
fn file_name(time: SystemTime) -> String {
    let millis = time
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis());
    format!("panic-{millis}.log")
}

/// One line per frame: the module name and the offset inside it, plus a function name
/// when the module has real symbols next to it.
fn format_frames(backtrace: Backtrace) -> String {
    let frames: Vec<BacktraceFrame> = backtrace.into();
    let mut text = String::new();
    for (index, mut frame) in frames.into_iter().enumerate() {
        let address = frame.ip() as usize;
        if address == 0 {
            continue;
        }
        let (path, offset) = module_of(address);
        let module = Path::new(&path)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(&path);
        let name = if has_symbols(&path) {
            frame.resolve();
            frame
                .symbols()
                .iter()
                .find_map(|symbol| symbol.name().map(|name| name.to_string()))
        } else {
            None
        };
        text.push_str(&format!("{index:4}: {module}+0x{offset:x}"));
        if let Some(name) = name {
            text.push(' ');
            text.push_str(&name);
        }
        text.push('\n');
    }
    text
}

fn module_of(address: usize) -> (String, usize) {
    platform::module_of(address).unwrap_or_else(|| ("unknown".to_owned(), address))
}

/// A `.pdb` next to the module is the debug build; without it any name would be a
/// nearby export rather than the function of the frame.
fn has_symbols(module: &str) -> bool {
    !module.is_empty() && Path::new(module).with_extension("pdb").is_file()
}

#[cfg(test)]
mod tests {
    use super::{Backtrace, file_name, format_frames, format_log, format_report};
    use std::time::{Duration, UNIX_EPOCH};

    #[test]
    fn the_report_has_the_message_place_and_backtrace() {
        let report = format_report(
            "cannot read Workspace while it is already being updated",
            "main",
            Some(("crates/app/ide/src/main.rs", 42, 9)),
            "   0: backtrace::capture",
        );
        assert_eq!(
            report,
            concat!(
                "panic: cannot read Workspace while it is already being updated\n",
                "thread: main\n",
                "location: crates/app/ide/src/main.rs:42:9\n",
                "backtrace:\n",
                "   0: backtrace::capture\n",
            )
        );
    }

    #[test]
    fn a_panic_without_a_location_says_so() {
        let report = format_report("out of memory", "worker", None, "none");
        assert!(report.contains("location: unknown"));
        assert!(report.contains("panic: out of memory"));
    }

    #[test]
    fn the_name_is_a_time_and_a_windows_file_name() {
        let name = file_name(UNIX_EPOCH + Duration::from_millis(1_765_000_123_456));
        assert_eq!(name, "panic-1765000123456.log");
        assert!(!name.contains(':'));
    }

    #[test]
    fn the_log_has_the_build_and_offset_frames_without_false_names() {
        // The panic is caught, so the test process survives; `panic_any` carries the
        // frames captured inside the panicking closure back to the test.
        let frames = std::panic::catch_unwind(|| {
            let frames = format_frames(Backtrace::new_unresolved());
            std::panic::panic_any(frames);
        })
        .expect_err("the panic must be caught");
        let frames = *frames.downcast::<String>().expect("the frames are text");
        assert!(!frames.is_empty(), "the backtrace has no frames");
        let report = format_log(
            "cannot read Workspace while it is already being updated",
            "main",
            Some(("crates/app/ide/src/panic_log.rs", 7, 9)),
            &frames,
        );
        let mut lines = report.lines();
        assert_eq!(
            lines.next(),
            Some(concat!("version: ", env!("CARGO_PKG_VERSION")))
        );
        assert!(
            lines
                .next()
                .is_some_and(|line| line.starts_with("commit: "))
        );
        assert!(lines.next().is_some_and(|line| line.starts_with("os: ")));
        assert!(report.contains("panic: cannot read Workspace"), "{report}");
        assert!(report.contains("backtrace:\n"), "{report}");
        assert!(!report.contains("tree_sitter_java_orchard"), "{report}");
        let frames = report
            .split_once("backtrace:\n")
            .map_or("", |(_, frames)| frames);
        let mut counted = 0;
        for line in frames.lines() {
            if line.trim().is_empty() {
                continue;
            }
            assert!(line.contains("+0x"), "a frame without an offset: {line}");
            counted += 1;
        }
        assert!(counted > 0, "the report has no frames");
    }
}

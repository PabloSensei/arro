//! The messages of the language server, as the Show Log button shows them.
//!
//! The sidecar keeps the stderr of the JVM in its own file. The errors of an import and
//! the other messages of the server come over the protocol, so they land here. A writer
//! thread keeps the thread that reads the protocol free of disk waits; when its queue is
//! full the lines are dropped, and the file says how many.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::thread::{self, JoinHandle};
use std::time::{SystemTime, UNIX_EPOCH};

/// The log that Show Log opens, next to the stderr of the JVM.
pub(crate) const MESSAGES_LOG: &str = "jdtls-messages.log";
/// The log of the previous start, kept beside the current one after a rotation.
pub(crate) const ROTATED_LOG: &str = "jdtls-messages.1.log";
/// The log rotates before the next write once it is this large.
pub(crate) const MAX_LOG_BYTES: u64 = 5 * 1024 * 1024;

/// The messages that may wait for the writer. A burst of them must not hold the reader.
const QUEUE: usize = 1024;

enum Event {
    Line(String),
    Section(String),
    Flush(SyncSender<()>),
}

/// A log that a reader of the protocol writes without waiting for the disk.
#[derive(Clone)]
pub(crate) struct ServerLog {
    sender: SyncSender<Event>,
    dropped: Arc<AtomicU64>,
    /// Held for its `Drop`, which joins the writer of this log.
    _writer: Arc<WriterHandle>,
}

impl ServerLog {
    /// Opens the log for appending and writes `heading` as its first line.
    pub(crate) fn open(path: &Path, heading: &str) -> io::Result<Self> {
        let mut file = append(path)?;
        write_all(&mut file, heading)?;
        let mut size = file.metadata().map_or(0, |metadata| metadata.len());
        let (sender, receiver) = sync_channel(QUEUE);
        let dropped = Arc::new(AtomicU64::new(0));
        let writer_dropped = dropped.clone();
        let log_path = path.to_path_buf();
        let writer = thread::Builder::new()
            .name("jdtls-messages".to_owned())
            .spawn(move || run(&log_path, &mut file, &mut size, receiver, writer_dropped))?;
        Ok(Self {
            sender,
            dropped,
            _writer: Arc::new(WriterHandle(Some(writer))),
        })
    }

    /// One message with the time it arrived and its level. A multi-line text stays whole.
    pub(crate) fn line(&self, level: &str, text: &str) {
        let line = format!("[{}] {level}: {text}\n", timestamp(SystemTime::now()));
        self.send(Event::Line(line));
    }

    /// A block of output under a heading, such as the stderr of the JVM.
    pub(crate) fn section(&self, heading: &str, text: &str) {
        let mut block = String::with_capacity(heading.len() + text.len() + 2);
        block.push_str(heading);
        block.push('\n');
        block.push_str(text);
        if !text.ends_with('\n') {
            block.push('\n');
        }
        self.send(Event::Section(block));
    }

    /// Waits until the writer stored everything queued before the call.
    pub(crate) fn flush(&self) {
        let (done, wait) = sync_channel(1);
        // The queue drains while this waits; the caller is not the reader of the protocol.
        if self.sender.send(Event::Flush(done)).is_ok() {
            let _ = wait.recv();
        }
    }

    fn send(&self, event: Event) {
        if let Err(TrySendError::Full(_)) = self.sender.try_send(event) {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Joins the writer when the last handle goes away, so an old start cannot write into
/// the log after the heading of a new one.
struct WriterHandle(Option<JoinHandle<()>>);

impl Drop for WriterHandle {
    fn drop(&mut self) {
        if let Some(writer) = self.0.take() {
            let _ = writer.join();
        }
    }
}

fn run(
    path: &Path,
    file: &mut File,
    size: &mut u64,
    receiver: Receiver<Event>,
    dropped: Arc<AtomicU64>,
) {
    for event in receiver {
        match event {
            Event::Line(text) | Event::Section(text) => {
                report_dropped(path, file, size, &dropped);
                write_event(path, file, size, &text);
            }
            Event::Flush(done) => {
                report_dropped(path, file, size, &dropped);
                let _ = file.flush();
                let _ = done.send(());
            }
        }
    }
}

/// Writes how many messages were dropped when the queue was full, and clears the count.
fn report_dropped(path: &Path, file: &mut File, size: &mut u64, dropped: &AtomicU64) {
    let lost = dropped.swap(0, Ordering::Relaxed);
    if lost == 0 {
        return;
    }
    let note = format!(
        "[{}] {lost} message(s) of the server log were dropped\n",
        timestamp(SystemTime::now())
    );
    write_event(path, file, size, &note);
}

fn write_event(path: &Path, file: &mut File, size: &mut u64, text: &str) {
    if *size >= MAX_LOG_BYTES {
        rotate(path, file, size);
    }
    if file.write_all(text.as_bytes()).is_ok() {
        *size += text.len() as u64;
    }
}

/// Moves the log to the name of the previous one and writes the next lines into a new
/// file. A failed rotation keeps the current file growing rather than losing output.
fn rotate(path: &Path, file: &mut File, size: &mut u64) {
    let rotated = path.with_file_name(ROTATED_LOG);
    let reopened = (|| {
        let _ = fs::remove_file(&rotated);
        fs::rename(path, &rotated)?;
        append(path)
    })();
    match reopened {
        Ok(new_file) => {
            *file = new_file;
            *size = 0;
        }
        Err(error) => {
            eprintln!("the server log is not rotated: {error}");
            *size = file.metadata().map_or(*size, |metadata| metadata.len());
        }
    }
}

fn append(path: &Path) -> io::Result<File> {
    OpenOptions::new().create(true).append(true).open(path)
}

fn write_all(file: &mut File, text: &str) -> io::Result<()> {
    file.write_all(text.as_bytes())?;
    file.write_all(b"\n")?;
    file.flush()
}

/// The time of a message as `YYYY-MM-DD HH:MM:SS` in UTC.
pub(crate) fn timestamp(time: SystemTime) -> String {
    let seconds = time
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs()) as i64;
    let days = seconds / 86_400;
    let remaining = seconds % 86_400;
    let (hour, minute, second) = (remaining / 3_600, remaining % 3_600 / 60, remaining % 60);
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02}")
}

/// Days since 1970-01-01 to a calendar date, the same arithmetic as the other logs.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let days = days + 719_468;
    let era = if days >= 0 { days } else { days - 146_096 } / 146_097;
    let day_of_era = days - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let mp = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (year + i64::from(month <= 2), month, day)
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, UNIX_EPOCH};

    use super::timestamp;

    #[test]
    fn the_time_is_a_calendar_stamp() {
        assert_eq!(
            timestamp(UNIX_EPOCH + Duration::from_millis(1_765_000_123_456)),
            "2025-12-06 05:48:43"
        );
    }
}

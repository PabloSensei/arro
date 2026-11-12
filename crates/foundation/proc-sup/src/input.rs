use std::io::Write;
use std::process::ChildStdin;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread;

/// One message for the standard input of a program.
enum Message {
    /// Text as it is; a line carries its own line break.
    Line(String),
    /// The program sees the end of its input.
    Eof,
}

/// The owner's end of the standard input of a program: it hands a line over at once, and
/// a thread writes it to the pipe, so a window never waits for the program to read.
#[derive(Clone)]
pub struct Input {
    messages: Sender<Message>,
    error: Arc<Mutex<Option<String>>>,
}

/// The writing end of one [`Input`], for the thread that owns the pipe of the program.
pub struct InputWriter {
    messages: Receiver<Message>,
    error: Arc<Mutex<Option<String>>>,
}

impl Input {
    /// The owner's end and the writer that feeds `stdin`.
    pub fn channel() -> (Self, InputWriter) {
        let (messages, receiver) = channel();
        let error = Arc::new(Mutex::new(None));
        (
            Self {
                messages,
                error: error.clone(),
            },
            InputWriter {
                messages: receiver,
                error,
            },
        )
    }

    /// A line with the line break that Enter adds. False when the program is gone.
    pub fn send_line(&self, line: &str) -> bool {
        self.send(Message::Line(format!("{line}\n")))
    }

    /// The program sees the end of its input. False when the program is gone.
    pub fn send_eof(&self) -> bool {
        self.send(Message::Eof)
    }

    /// Why the last write failed, since the last call.
    pub fn take_error(&self) -> Option<String> {
        self.error
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
    }

    fn send(&self, message: Message) -> bool {
        if self.messages.send(message).is_ok() {
            return true;
        }
        self.report("the program has exited");
        false
    }

    fn report(&self, reason: &str) {
        let mut slot = self
            .error
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *slot = Some(reason.to_owned());
    }
}

impl InputWriter {
    /// Writes the messages to `stdin` on a thread of its own, until the end of the input,
    /// the program is gone or the owner of the [`Input`] is.
    pub fn start(self, stdin: ChildStdin) {
        thread::spawn(move || write_messages(stdin, self.messages, self.error));
    }
}

fn write_messages(
    mut stdin: ChildStdin,
    messages: Receiver<Message>,
    error: Arc<Mutex<Option<String>>>,
) {
    while let Ok(message) = messages.recv() {
        let result = match message {
            Message::Line(text) => stdin
                .write_all(text.as_bytes())
                .and_then(|()| stdin.flush()),
            Message::Eof => return,
        };
        if let Err(failure) = result {
            let mut slot = error
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            *slot = Some(failure.to_string());
            return;
        }
    }
}
